use super::definitions::{self, FieldDef};

/// Sort fields by canonical order (type_code, then field_code/nth).
pub fn sort_fields_canonical(field_names: &mut Vec<String>) {
    field_names.sort_by(|a, b| {
        let a_def = definitions::get_field(a);
        let b_def = definitions::get_field(b);
        match (a_def, b_def) {
            (Some(a), Some(b)) => a.sort_key().cmp(&b.sort_key()),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    });
}

/// Check if a field should be included in serialization.
pub fn should_serialize(field: &FieldDef, include_all: bool) -> bool {
    if !field.is_serialized {
        return false;
    }
    if !include_all && !field.is_signing_field {
        return false;
    }
    true
}
