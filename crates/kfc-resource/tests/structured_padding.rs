//! Genuine serializer regressions for unordered reflected fields and padding.
use indexmap::IndexMap;
use kfc::{
    hash::fnv,
    reflection::{
        PrimitiveType, StructFieldMetadata, TypeFlags, TypeIndex, TypeMetadata, TypeRegistry,
    },
};
use kfc_resource::value::Value;
use std::io::{Cursor, Seek};

fn metadata(index: usize, name: &str, size: u32, primitive: PrimitiveType) -> TypeMetadata {
    TypeMetadata {
        index: TypeIndex::new(index),
        name: name.into(),
        qualified_name: format!("test::{name}"),
        impact_name: name.into(),
        namespace: vec!["test".into()],
        inner_type: None,
        size,
        alignment: 4,
        element_alignment: 4,
        field_count: 0,
        primitive_type: primitive,
        flags: TypeFlags::NONE,
        name_hash: fnv(name),
        impact_hash: fnv(name),
        qualified_hash: fnv(&format!("test::{name}")),
        internal_hash: index as u32 + 100,
        struct_fields: IndexMap::new(),
        enum_fields: IndexMap::new(),
        default_value: None,
        attributes: IndexMap::new(),
    }
}
fn field(name: &str, offset: u64) -> StructFieldMetadata {
    StructFieldMetadata {
        name: name.into(),
        r#type: TypeIndex::new(0),
        data_offset: offset,
        attributes: IndexMap::new(),
    }
}
fn fixture() -> TypeRegistry {
    let scalar = metadata(0, "U32", 4, PrimitiveType::UInt32);
    let mut hash = metadata(1, "ContentHash", 16, PrimitiveType::Struct);
    // Exact physical ordering problem in keen::ContentHash: the last reflected
    // field is first in memory. Its last-word high byte must not be padding.
    for (name, offset) in [("hash0", 4), ("hash1", 8), ("hash2", 12), ("size", 0)] {
        hash.struct_fields.insert(name.into(), field(name, offset));
    }
    hash.field_count = 4;
    let mut array = metadata(2, "HashArray", 8, PrimitiveType::BlobArray);
    array.inner_type = Some(TypeIndex::new(1));
    let mut padded = metadata(3, "Padded", 24, PrimitiveType::Struct);
    padded
        .struct_fields
        .insert("first".into(), field("first", 0));
    padded.field_count = 1;
    serde_json::from_value(serde_json::json!({"version":"synthetic-padding-regression",
        "types":[scalar,hash,array,padded]}))
    .unwrap()
}
fn hash() -> Value {
    Value::Struct(Box::new(IndexMap::from([
        ("hash0".into(), Value::UInt(0x91a2b3c4)),
        ("hash1".into(), Value::UInt(0xa5b6c7d8)),
        ("hash2".into(), Value::UInt(0xf1234567)),
        ("size".into(), Value::UInt(234)),
    ])))
}
fn bytes() -> Vec<u8> {
    [234u32, 0x91a2b3c4, 0xa5b6c7d8, 0xf1234567]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
}

#[test]
fn unordered_content_hash_preserves_every_byte_and_roundtrips() {
    let r = fixture();
    let t = r.get(TypeIndex::new(1)).unwrap();
    let value = hash();
    let raw = value.to_bytes(&r, t).unwrap();
    assert_eq!(raw, bytes());
    assert_eq!(Value::from_bytes(&r, t, &raw).unwrap(), value);
}

#[test]
fn nested_blob_array_keeps_final_hash_word_and_valid_relative_layout() {
    let r = fixture();
    let t = r.get(TypeIndex::new(2)).unwrap();
    let value = Value::Array(vec![hash(), hash()]);
    let raw = value.to_bytes(&r, t).unwrap();
    assert_eq!(raw.len(), 40);
    assert_eq!(&raw[8..24], bytes());
    assert_eq!(&raw[24..40], bytes());
    assert_eq!(Value::from_bytes(&r, t, &raw).unwrap(), value);
}

#[test]
fn genuine_trailing_padding_grows_output_and_leaves_cursor_at_inline_end() {
    let r = fixture();
    let t = r.get(TypeIndex::new(3)).unwrap();
    let value = Value::Struct(Box::new(IndexMap::from([(
        "first".into(),
        Value::UInt(0xabcdef01),
    )])));
    let mut out = Cursor::new(Vec::new());
    value.write(&r, t, &mut out).unwrap();
    assert_eq!(out.stream_position().unwrap(), 24);
    let raw = out.into_inner();
    assert_eq!(raw.len(), 24);
    assert_eq!(&raw[..4], 0xabcdef01u32.to_le_bytes());
    assert!(raw[4..].iter().all(|&byte| byte == 0));
}

#[test]
fn existing_suffix_is_preserved_and_logical_cursor_does_not_jump_to_eof() {
    let r = fixture();
    let t = r.get(TypeIndex::new(1)).unwrap();
    let mut out = Cursor::new(vec![0x5a; 32]);
    hash().write(&r, t, &mut out).unwrap();
    assert_eq!(out.stream_position().unwrap(), 16);
    let raw = out.into_inner();
    assert_eq!(&raw[..16], bytes());
    assert_eq!(&raw[16..], &[0x5a; 16]);
}
