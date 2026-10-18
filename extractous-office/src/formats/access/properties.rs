//! Jackcess's `PropertyMaps`: the properties a database object keeps in the `LvProp` column of
//! its row of the system catalog, as maps of the object itself, its columns and its indexes.

use super::buffer::{Buffer, Thrown, ARRAY_INDEX_OUT_OF_BOUNDS, INDEX_OUT_OF_BOUNDS};
use super::column::{read_property, DataType, Value};
use super::format::Charset;
use super::pages::Pages;

const PROPERTY_MAP_TYPES: [&[u8]; 2] = [b"MR2\0", b"KKD\0"];
const PROPERTY_NAME_LIST: i16 = 0x80;
const DEFAULT_PROPERTY_VALUE_LIST: i16 = 0x00;
const COLUMN_PROPERTY_VALUE_LIST: i16 = 0x01;
const GUID_PROPERTY: &str = "GUID";
const GUID_SIZE: i16 = 16;

/// A property: its name as last stored, its declared type and its value.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Property {
    pub(super) name: String,
    pub(super) data_type: DataType,
    pub(super) value: Option<Value>,
}

/// A `PropertyMapImpl`: properties by their upper-cased names, in the order first stored.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct PropertyMap {
    properties: Vec<(String, Property)>,
}

impl PropertyMap {
    fn put(&mut self, property: Property) {
        let key = lookup_name(&property.name);
        match self.properties.iter_mut().find(|(name, _)| *name == key) {
            Some((_, existing)) => *existing = property,
            None => self.properties.push((key, property)),
        }
    }

    /// `get`: the property named so, whatever its case.
    pub(super) fn get(&self, name: &str) -> Option<&Property> {
        let key = lookup_name(name);
        self.properties
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, property)| property)
    }

    /// `getValue`.
    pub(super) fn value(&self, name: &str) -> Option<&Value> {
        self.get(name).and_then(|property| property.value.as_ref())
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &Property> {
        self.properties.iter().map(|(_, property)| property)
    }
}

/// `DatabaseImpl.toLookupName`.
pub(super) fn lookup_name(name: &str) -> String {
    crate::java::to_upper_case(name)
}

/// The maps of an object, by their upper-cased names and block types.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct PropertyMaps {
    maps: Vec<((String, i16), PropertyMap)>,
}

impl PropertyMaps {
    fn map_mut(&mut self, name: &str, block_type: i16) -> &mut PropertyMap {
        let key = (lookup_name(name), block_type);
        let index = match self.maps.iter().position(|(k, _)| *k == key) {
            Some(index) => index,
            None => {
                self.maps.push((key, PropertyMap::default()));
                self.maps.len() - 1
            }
        };
        &mut self.maps[index].1
    }

    fn map(&self, name: &str, block_type: i16) -> Option<&PropertyMap> {
        let key = (lookup_name(name), block_type);
        self.maps
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, map)| map)
    }

    /// `getDefault`: the object's own properties.
    pub(super) fn default_map(&self) -> PropertyMap {
        self.map("", DEFAULT_PROPERTY_VALUE_LIST)
            .cloned()
            .unwrap_or_default()
    }

    /// `get(name)`: a column's properties.
    pub(super) fn column(&self, name: &str) -> PropertyMap {
        self.map(name, COLUMN_PROPERTY_VALUE_LIST)
            .cloned()
            .unwrap_or_default()
    }

    /// `PropertyMaps.Handler.read`.
    pub(super) fn read(bytes: Option<&[u8]>, pages: &Pages<'_>) -> Result<Self, Thrown> {
        let mut maps = Self::default();
        let Some(bytes) = bytes.filter(|bytes| !bytes.is_empty()) else {
            return Ok(maps);
        };
        let charset = pages.charset;
        let mut buffer = Buffer::new(bytes);
        let Some(map_type) = PROPERTY_MAP_TYPES
            .iter()
            .find(|map_type| bytes.starts_with(map_type))
        else {
            let shown: String = bytes
                .iter()
                .take(4)
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            return Err(Thrown::io(format!("Unknown property map type {shown}")));
        };
        buffer.forward(map_type.len())?;
        let mut names: Option<Vec<String>> = None;
        while buffer.has_remaining() {
            let length = buffer.i32()?;
            let block_type = buffer.i16()?;
            let end = buffer.position() as i64 + i64::from(length) - 6;
            let mut block = buffer;
            block.set_limit(end)?;
            block.set_position(buffer.position() as i64)?;
            if block_type == PROPERTY_NAME_LIST {
                let mut list = Vec::new();
                while block.has_remaining() {
                    list.push(read_name(&mut block, charset)?);
                }
                names = Some(list);
            } else {
                maps.read_values(&mut block, names.as_deref(), block_type, charset)?;
            }
            buffer.set_position(end)?;
        }
        Ok(maps)
    }

    /// `readPropertyValues`: a block's map name, then its properties.
    fn read_values(
        &mut self,
        block: &mut Buffer<'_>,
        names: Option<&[String]>,
        block_type: i16,
        charset: Charset,
    ) -> Result<(), Thrown> {
        let mut map_name = String::new();
        if block.has_remaining() {
            let name_block_length = block.i32()?;
            let end = block.position() as i64 + i64::from(name_block_length) - 4;
            if name_block_length > 6 {
                map_name = read_name(block, charset)?;
            }
            block.set_position(end)?;
        }
        let map = self.map_mut(&map_name, block_type);
        while block.has_remaining() {
            let value_length = block.i16()?;
            let end = block.position() as i64 + i64::from(value_length) - 2;
            let _flags = block.u8()?;
            let data_type = DataType::from_byte_or_throw(block.u8()?)?;
            let name_index = block.i16()?;
            let data_size = block.i16()?;
            let names = names.ok_or_else(Thrown::null_pointer)?;
            let name = usize::try_from(name_index)
                .ok()
                .and_then(|index| names.get(index))
                .ok_or_else(|| {
                    Thrown::index_out_of_bounds(
                        INDEX_OUT_OF_BOUNDS,
                        Some(format!(
                            "Index {name_index} out of bounds for length {}",
                            names.len()
                        )),
                    )
                })?
                .clone();
            let data = block.take(i64::from(data_size))?;
            let pseudo_guid = data_type == DataType::Binary
                && data_size == GUID_SIZE
                && crate::java::equals_ignore_case(&name, GUID_PROPERTY);
            let value = read_value(data_type, pseudo_guid, data, charset)?;
            map.put(Property {
                name,
                data_type,
                value,
            });
            block.set_position(end)?;
        }
        Ok(())
    }
}

/// `readPropName`: a name after its length in bytes.
fn read_name(buffer: &mut Buffer<'_>, charset: Charset) -> Result<String, Thrown> {
    let length = buffer.i16()?;
    let bytes = buffer.take(i64::from(length))?;
    Ok(charset.decode(bytes))
}

/// `PropColumn.read`: memo read as text, OLE as binary, a binary GUID property as a GUID, a
/// boolean by its first byte, any other by its type.
fn read_value(
    data_type: DataType,
    pseudo_guid: bool,
    data: &[u8],
    charset: Charset,
) -> Result<Option<Value>, Thrown> {
    let read_as = match data_type {
        _ if pseudo_guid => DataType::Guid,
        DataType::Memo => DataType::Text,
        DataType::Ole => DataType::Binary,
        DataType::Boolean => {
            let first = data.first().ok_or_else(|| {
                Thrown::index_out_of_bounds(
                    ARRAY_INDEX_OUT_OF_BOUNDS,
                    Some("Index 0 out of bounds for length 0".to_owned()),
                )
            })?;
            return Ok(Some(Value::Boolean(*first != 0)));
        }
        other => other,
    };
    read_property(read_as, data, charset).map(Some)
}
