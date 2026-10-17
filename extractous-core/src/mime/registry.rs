//! Tika's `MediaTypeRegistry`: the known types, their aliases and what each specializes.

use std::borrow::Cow;

use super::media_type::MediaType;
use super::sorted_get;

/// Pairs of printed media types, sorted by the first, which is unique.
type Pairs = Cow<'static, [(&'static str, &'static str)]>;

/// The types `tika-mimetypes.xml` declares, the aliases it maps to them, and the supertype each
/// declares, by their printed forms.
#[derive(Debug, PartialEq)]
pub(crate) struct Registry {
    /// Each type to itself and each alias to its type.
    canonical: Pairs,
    supertypes: Pairs,
}

impl Registry {
    /// The registry of the generated tables.
    pub(crate) const fn new(
        canonical: &'static [(&'static str, &'static str)],
        supertypes: &'static [(&'static str, &'static str)],
    ) -> Self {
        Registry {
            canonical: Cow::Borrowed(canonical),
            supertypes: Cow::Borrowed(supertypes),
        }
    }

    /// `normalize`: the canonical type an alias names, its parameters kept.
    pub(crate) fn normalize(&self, kind: &MediaType) -> MediaType {
        match sorted_get(&self.canonical, kind.base_str()) {
            None => kind.clone(),
            Some(canonical) if kind.has_parameters() => {
                MediaType::named(canonical).with_parameters_of(kind)
            }
            Some(canonical) => MediaType::named(canonical),
        }
    }

    /// Whether `kind`, or its base type, is a type or an alias the registry declares.
    pub(crate) fn declares(&self, kind: &MediaType) -> bool {
        sorted_get(&self.canonical, kind.as_str()).is_some()
            || sorted_get(&self.canonical, kind.base_str()).is_some()
    }

    /// `isSpecializationOf`: whether `a` descends from `b`.
    pub(crate) fn is_specialization_of(&self, a: &MediaType, b: &MediaType) -> bool {
        let mut current = self.supertype(a);
        while let Some(kind) = current {
            if kind == *b {
                return true;
            }
            current = self.supertype(&kind);
        }
        false
    }

    /// `getSupertype`: the declared supertype, else the base type of a parameterized type, else
    /// the supertype the subtype's suffix or the top-level type implies.
    pub(crate) fn supertype(&self, kind: &MediaType) -> Option<MediaType> {
        if let Some(supertype) = sorted_get(&self.supertypes, kind.as_str()) {
            return Some(MediaType::named(supertype));
        }
        if kind.has_parameters() {
            return Some(kind.base_type());
        }
        let implied = if kind.subtype().ends_with("+xml") {
            "application/xml"
        } else if kind.subtype().ends_with("+zip") {
            "application/zip"
        } else if kind.kind() == "text" && kind.as_str() != "text/plain" {
            "text/plain"
        } else if kind.kind().contains("empty") && kind.as_str() != "application/x-empty" {
            "application/x-empty"
        } else if kind.as_str() != "application/octet-stream" {
            "application/octet-stream"
        } else {
            return None;
        };
        Some(MediaType::named(implied))
    }
}

/// The registry as the generator builds it, from the types and aliases it reads.
#[cfg(test)]
impl Registry {
    pub(crate) fn empty() -> Self {
        Self::new(&[], &[])
    }

    pub(crate) fn add_type(&mut self, kind: &MediaType) {
        Self::insert(&mut self.canonical, kind, kind);
    }

    pub(crate) fn add_alias(&mut self, kind: &MediaType, alias: &MediaType) {
        Self::insert(&mut self.canonical, alias, kind);
    }

    pub(crate) fn add_supertype(&mut self, kind: &MediaType, supertype: &MediaType) {
        Self::insert(&mut self.supertypes, kind, supertype);
    }

    /// The canonical pairs and the supertype pairs.
    pub(crate) fn pairs(&self) -> [&[(&'static str, &'static str)]; 2] {
        [&self.canonical, &self.supertypes]
    }

    /// Maps `key` to `value`, replacing what `key` mapped to, as `HashMap.put` does.
    fn insert(pairs: &mut Pairs, key: &MediaType, value: &MediaType) {
        let leaked = |kind: &MediaType| -> &'static str { kind.as_str().to_owned().leak() };
        let pairs = pairs.to_mut();
        match pairs.binary_search_by(|(known, _)| (*known).cmp(key.as_str())) {
            Ok(index) => pairs[index].1 = leaked(value),
            Err(index) => pairs.insert(index, (leaked(key), leaked(value))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Registry {
        let mut registry = Registry::empty();
        registry.add_type(&MediaType::named("application/x-tika-msoffice"));
        registry.add_type(&MediaType::named("application/msword"));
        registry.add_supertype(
            &MediaType::named("application/msword"),
            &MediaType::named("application/x-tika-msoffice"),
        );
        registry.add_alias(
            &MediaType::named("application/msword"),
            &MediaType::named("application/doc"),
        );
        registry
    }

    #[test]
    fn follows_declared_and_implied_supertypes() {
        let registry = registry();
        let doc = MediaType::named("application/msword");
        let ole = MediaType::named("application/x-tika-msoffice");
        let octet = MediaType::named("application/octet-stream");
        assert!(registry.is_specialization_of(&doc, &ole));
        assert!(registry.is_specialization_of(&doc, &octet));
        assert!(!registry.is_specialization_of(&ole, &doc));
        assert!(!registry.is_specialization_of(&octet, &octet));
        assert!(!registry.is_specialization_of(
            &MediaType::named("image/svg+xml"),
            &MediaType::named("text/plain")
        ));
        assert!(registry.is_specialization_of(
            &MediaType::named("text/x-java"),
            &MediaType::named("text/plain")
        ));
        assert!(registry.is_specialization_of(
            &MediaType::named("application/epub+zip"),
            &MediaType::named("application/zip")
        ));
    }

    #[test]
    fn normalizes_aliases() {
        let registry = registry();
        assert_eq!(
            registry
                .normalize(&MediaType::named("application/doc"))
                .as_str(),
            "application/msword"
        );
        assert_eq!(
            registry
                .normalize(&MediaType::named("application/doc; a=b"))
                .as_str(),
            "application/msword; a=b"
        );
    }
}
