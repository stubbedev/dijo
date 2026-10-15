//! Tika's `MediaTypeRegistry`: the known types, their aliases and what each specializes.

use std::collections::HashMap;

use super::media_type::MediaType;

/// The types `tika-mimetypes.xml` declares, the aliases it maps to them, and the supertype each
/// declares.
#[derive(Debug, Default)]
pub(crate) struct Registry {
    canonical: HashMap<MediaType, MediaType>,
    supertypes: HashMap<MediaType, MediaType>,
}

impl Registry {
    pub(crate) fn add_type(&mut self, kind: MediaType) {
        self.canonical.insert(kind.clone(), kind);
    }

    pub(crate) fn add_alias(&mut self, kind: MediaType, alias: MediaType) {
        self.canonical.insert(alias, kind);
    }

    pub(crate) fn add_supertype(&mut self, kind: MediaType, supertype: MediaType) {
        self.supertypes.insert(kind, supertype);
    }

    /// `normalize`: the canonical type an alias names, its parameters kept.
    pub(crate) fn normalize(&self, kind: &MediaType) -> MediaType {
        match self.canonical.get(&kind.base_type()) {
            None => kind.clone(),
            Some(canonical) if kind.has_parameters() => canonical.with_parameters_of(kind),
            Some(canonical) => canonical.clone(),
        }
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
        if let Some(supertype) = self.supertypes.get(kind) {
            return Some(supertype.clone());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Registry {
        let mut registry = Registry::default();
        registry.add_type(MediaType::named("application/x-tika-msoffice"));
        registry.add_type(MediaType::named("application/msword"));
        registry.add_supertype(
            MediaType::named("application/msword"),
            MediaType::named("application/x-tika-msoffice"),
        );
        registry.add_alias(
            MediaType::named("application/msword"),
            MediaType::named("application/doc"),
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
