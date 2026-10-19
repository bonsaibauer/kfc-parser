//! Name-based boundary used by ShroudForge to construct physical KFC resource identifiers.
//! These helpers only query existing metadata. They do not change container parsing or EML.
use crate::{
    container::KFCFile,
    guid::{Guid, ResourceId},
    reflection::{LookupKey, TypeIndex, TypeMetadata, TypeRegistry},
};

/// Resolves a qualified type name and combines its hash with a GUID and part index.
/// ShroudForge uses this for asset bindings. EML continues to use its existing code path.
pub fn resource_id(
    registry: &TypeRegistry,
    qualified_name: &str,
    guid: Guid,
    part: u32,
) -> Option<ResourceId> {
    registry
        .get_by_name(LookupKey::Qualified(qualified_name))
        .map(|metadata| ResourceId::new(guid, metadata.qualified_hash, part))
}

/// Builds the same identifier from a registry type index already held by ShroudForge.
/// This is an additive lookup helper and does not affect parser behavior or EML.
pub fn resource_id_for_type(
    registry: &TypeRegistry,
    type_index: TypeIndex,
    guid: Guid,
    part: u32,
) -> Option<ResourceId> {
    registry
        .get(type_index)
        .and_then(|metadata| resource_id(registry, &metadata.qualified_name, guid, part))
}

/// Resolves the reflected type for an existing resource identifier.
/// ShroudForge uses the result in its runtime bindings. EML is unchanged.
pub fn type_for_resource<'a>(
    registry: &'a TypeRegistry,
    resource: &ResourceId,
) -> Option<&'a TypeMetadata> {
    registry.get_by_hash(LookupKey::Qualified(resource.type_hash()))
}

/// Compares resource type hashes without changing either identifier or parser state.
/// This supports ShroudForge lookups only. EML does not use this helper.
pub fn same_type(left: &ResourceId, right: &ResourceId) -> bool {
    left.type_hash() == right.type_hash()
}

/// Returns the resources of a named reflected type from an already parsed KFC file.
/// This is a read-only ShroudForge lookup. It leaves KFC parsing and EML unchanged.
pub fn resources_by_type(
    file: &KFCFile,
    registry: &TypeRegistry,
    qualified_name: &str,
) -> Vec<ResourceId> {
    let Some(metadata) = registry.get_by_name(LookupKey::Qualified(qualified_name)) else {
        return Vec::new();
    };
    file.resources_by_type(metadata.qualified_hash)
        .copied()
        .collect()
}

/// Maps file resource type hashes back to known registry indices for ShroudForge.
/// It only reads existing parser results and has no effect on EML.
pub fn resource_type_indices(file: &KFCFile, registry: &TypeRegistry) -> Vec<TypeIndex> {
    file.resource_types()
        .filter_map(|identifier| registry.get_by_hash(LookupKey::Qualified(identifier)))
        .map(|metadata| metadata.index)
        .collect()
}
