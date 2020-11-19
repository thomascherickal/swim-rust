// Copyright 2015-2020 SWIM.AI inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use proc_macro2::Ident;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Attribute, DeriveInput, Lit, Meta, NestedMeta, Variant};

use macro_helpers::{get_attribute_meta, CompoundTypeKind, Context, Label, StructureKind, Symbol};
use syn::export::ToTokens;

pub const FORM_PATH: Symbol = Symbol("form");
pub const HEADER_PATH: Symbol = Symbol("header");
pub const ATTR_PATH: Symbol = Symbol("attr");
pub const SLOT_PATH: Symbol = Symbol("slot");
pub const BODY_PATH: Symbol = Symbol("body");
pub const HEADER_BODY_PATH: Symbol = Symbol("header_body");
pub const RENAME_PATH: Symbol = Symbol("rename");
pub const TAG_PATH: Symbol = Symbol("tag");
pub const SKIP_PATH: Symbol = Symbol("skip");
pub const SCHEMA_PATH: Symbol = Symbol("schema");

/// An enumeration representing the contents of an input.
#[derive(Clone)]
pub enum TypeContents<'t, D, F> {
    /// An enumeration input. Containing a vector of enumeration variants.
    Enum(Vec<EnumVariant<'t, D, F>>),
    /// A struct input containing its representation.
    Struct(StructRepr<'t, D, F>),
}

/// A representation of a parsed struct from the AST.
#[derive(Clone)]
pub struct StructRepr<'t, D, F> {
    pub input: &'t DeriveInput,
    /// The struct's type: tuple, named, unit.
    pub compound_type: CompoundTypeKind,
    /// The field members of the struct.
    pub fields: Vec<F>,
    /// A form descriptor
    pub descriptor: D,
}

/// A representation of a parsed enumeration from the AST.
#[derive(Clone)]
pub struct EnumVariant<'t, D, F> {
    pub syn_variant: &'t Variant,
    /// The name of the variant.
    pub name: Label,
    /// The variant's type: tuple, named, unit.
    pub compound_type: CompoundTypeKind,
    /// The field members of the variant.
    pub fields: Vec<F>,
    /// A form descriptor
    pub descriptor: D,
}

/// A representation of a parsed field for a form from the AST.
#[derive(Clone)]
pub struct FormField<'a> {
    /// The original field from the [`DeriveInput`].
    pub original: &'a syn::Field,
    /// The name of the field.
    pub label: Label,
    /// The kind of the field from its attribute.
    pub kind: FieldKind,
}

impl<'a> FormField<'a> {
    pub fn is_skipped(&self) -> bool {
        self.kind == FieldKind::Skip && !self.label.is_foreign()
    }

    pub fn is_attr(&self) -> bool {
        self.kind == FieldKind::Attr
    }

    pub fn is_slot(&self) -> bool {
        self.kind == FieldKind::Slot
    }

    pub fn is_body(&self) -> bool {
        self.kind == FieldKind::Body
    }

    pub fn is_header_body(&self) -> bool {
        self.kind == FieldKind::HeaderBody
    }

    pub fn is_header(&self) -> bool {
        self.kind == FieldKind::Header
    }
}

/// Parse a structure's fields from the [`DeriveInput`]'s fields. Returns the type of the fields,
/// parsed fields that contain a name and kind, and a derived [`FieldManifest]`. Any errors
/// encountered are added to the [`Context]`.
pub fn parse_struct<'a>(
    context: &mut Context,
    fields: &'a syn::Fields,
    container_label: &mut Label,
    structure_kind: StructureKind,
) -> (CompoundTypeKind, Vec<FormField<'a>>, FieldManifest) {
    match fields {
        syn::Fields::Named(fields) => {
            let (fields, manifest) =
                fields_from_ast(context, &fields.named, container_label, structure_kind);
            (CompoundTypeKind::Struct, fields, manifest)
        }
        syn::Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
            let (fields, manifest) =
                fields_from_ast(context, &fields.unnamed, container_label, structure_kind);
            (CompoundTypeKind::NewType, fields, manifest)
        }
        syn::Fields::Unnamed(fields) => {
            let (fields, manifest) =
                fields_from_ast(context, &fields.unnamed, container_label, structure_kind);
            (CompoundTypeKind::Tuple, fields, manifest)
        }
        syn::Fields::Unit => (CompoundTypeKind::Unit, Vec::new(), FieldManifest::default()),
    }
}

/// Parses an AST of fields and produces a vector of fields that contain their final identities
/// and their kind as well as producing a [`FieldManifest`].
pub fn fields_from_ast<'t>(
    ctx: &mut Context,
    fields: &'t Punctuated<syn::Field, syn::Token![,]>,
    container_label: &mut Label,
    structure_type: StructureKind,
) -> (Vec<FormField<'t>>, FieldManifest) {
    let mut name_opt = None;

    let mut manifest = FieldManifest::default();
    let fields = fields
        .iter()
        .enumerate()
        .map(|(index, original)| {
            let mut kind_opt = None;
            let mut set_kind = |kind, ctx: &mut Context, protected: bool| {
                if protected {
                    match kind_opt {
                        Some(_) => ctx.error_spanned_by(
                            original,
                            "A field can be marked by at most one kind.",
                        ),
                        None => kind_opt = Some(kind),
                    }
                } else {
                    kind_opt = Some(kind);
                }
            };

            let mut renamed = None;

            original.attrs.get_attributes(ctx, FORM_PATH).iter().fold(
                &mut manifest,
                |mut manifest, meta| {
                    match meta {
                        NestedMeta::Meta(Meta::Path(path)) if path == HEADER_PATH => {
                            manifest.has_header_fields = true;
                            set_kind(FieldKind::Header, ctx, true);
                        }
                        NestedMeta::Meta(Meta::Path(path)) if path == ATTR_PATH => {
                            set_kind(FieldKind::Attr, ctx, true);
                            manifest.has_attr_fields = true;
                        }
                        NestedMeta::Meta(Meta::Path(path)) if path == SLOT_PATH => {
                            set_kind(FieldKind::Slot, ctx, true);

                            if manifest.replaces_body {
                                manifest.has_header_fields = true;
                            } else {
                                manifest.has_slot_fields = true;
                            }
                        }
                        NestedMeta::Meta(Meta::Path(path)) if path == BODY_PATH => {
                            set_kind(FieldKind::Body, ctx, true);

                            if manifest.replaces_body {
                                ctx.error_spanned_by(
                                    path,
                                    "At most one field can replace the body.",
                                )
                            } else if manifest.has_slot_fields {
                                manifest.has_slot_fields = false;
                                manifest.has_header_fields = true;
                                manifest.replaces_body = true;
                            } else {
                                manifest.replaces_body = true;
                            }
                        }
                        NestedMeta::Meta(Meta::Path(path)) if path == HEADER_BODY_PATH => {
                            set_kind(FieldKind::HeaderBody, ctx, true);

                            if manifest.header_body {
                                ctx.error_spanned_by(
                                    path,
                                    "At most one field can replace the tag attribute body.",
                                )
                            } else {
                                manifest.header_body = true;
                            }
                        }
                        NestedMeta::Meta(Meta::NameValue(name)) if name.path == RENAME_PATH => {
                            match &name.lit {
                                Lit::Str(s) => {
                                    let old_ident = original.ident.clone().unwrap_or_else(|| {
                                        Ident::new(&format!("__self_{}", index), original.span())
                                    });

                                    renamed = Some(Label::Renamed {
                                        new_label: s.value(),
                                        old_label: old_ident,
                                    });
                                }
                                _ => ctx.error_spanned_by(meta, "Expected string argument"),
                            }
                        }
                        NestedMeta::Meta(Meta::Path(path)) if path == SKIP_PATH => {
                            set_kind(FieldKind::Skip, ctx, true);
                        }
                        NestedMeta::Meta(Meta::List(list)) if list.path == SCHEMA_PATH => {
                            // no-op as this is parsed by the validated form derive macro
                        }
                        NestedMeta::Meta(Meta::Path(path)) if path == TAG_PATH => match name_opt {
                            Some(_) => ctx.error_spanned_by(path, "Duplicate tag"),
                            None => match &original.ident {
                                Some(ident) => {
                                    if container_label.is_modified() {
                                        ctx.error_spanned_by(path, "Cannot apply a tag using a field when one has already been applied at the container level");   
                                    } else if structure_type.is_enum(){
                                        ctx.error_spanned_by(path, "Deriving tags is not supported by enumerations. Use #[form(tag = \"....\")] on this variant instead")
                                    } else {
                                        name_opt = Some(Label::Foreign(ident.clone(), original.ty.to_token_stream(), container_label.original()));
                                        set_kind(FieldKind::Tagged, ctx, false);
                                    }
                                }
                                None => ctx.error_spanned_by(path, "Invalid on anonymous fields"),
                            },
                        },
                        _ => ctx.error_spanned_by(meta, "Unknown attribute"),
                    }

                    manifest
                },
            );

            let name = renamed.unwrap_or_else(|| match &original.ident {
                Some(ident) => Label::Unmodified(ident.clone()),
                None => Label::Anonymous(index.into()),
            });

            let kind = kind_opt.unwrap_or(FieldKind::Slot);

            if let (Label::Anonymous(_), FieldKind::Attr) = (&name, &kind) {
                ctx.error_spanned_by(
                    original,
                    "An unnamed field cannot be promoted to an attribute.",
                )
            }

            FormField {
                original,
                label: name,
                kind: kind_opt.unwrap_or(FieldKind::Slot),
            }
        })
        .collect();

    if let Some(name) = name_opt {
        *container_label = name;
    }

    (fields, manifest)
}

/// A trait for retrieving attributes on a field or compound type that are prefixed by the provided
/// [`symbol`]. For example calling this on a [`DeriveInput`] that represents the following:
///```compile_fail
///struct Person {
///    #[form(skip)]
///    name: String,
///    age: i32,
/// }
///```
/// will return a [`Vector`] that contains the [`NestedMeta`] for the field [`name`].
pub trait Attributes {
    /// Returns a vector of [`NestedMeta`] for all attributes that contain a path that matches the
    /// provided symbol or an empty vector if there are no matches.
    fn get_attributes(&self, ctx: &mut Context, symbol: Symbol) -> Vec<NestedMeta>;
}

impl Attributes for Vec<Attribute> {
    fn get_attributes(&self, ctx: &mut Context, symbol: Symbol) -> Vec<NestedMeta> {
        self.iter()
            .flat_map(|a| get_attribute_meta(ctx, a, symbol))
            .flatten()
            .collect()
    }
}

/// Enumeration of ways in which fields can be serialized in Recon documents. Unannotated fields
/// are assumed to be annotated as [`Item::Slot`].
#[derive(PartialEq, Debug, Eq, Hash, Copy, Clone)]
pub enum FieldKind {
    /// The field should be written as a slot in the tag attribute.
    Header,
    /// The field should be written as an attribute.
    Attr,
    /// The field should be written as a slot in the main body (or the header if another field is
    /// marked as [`FieldKind::Body`]
    Slot,
    /// The field should be used to form the entire body of the record, all other fields that are
    /// marked as slots will be promoted to headers. At most one field may be marked with this.
    Body,
    /// The field should be moved into the body of the tag attribute (unlabelled). If there are no
    /// header fields it will form the entire body of the tag, otherwise it will be the first item
    /// of the tag body. At most one field may be marked with this.
    HeaderBody,
    /// The field will be ignored during transformations. The decorated field must implement
    /// [`Default`].
    Skip,
    Tagged,
}

impl Default for FieldKind {
    fn default() -> Self {
        FieldKind::Slot
    }
}

/// A structure representing what fields in the compound type are annotated with.
#[derive(Default, Clone, Debug)]
pub struct FieldManifest {
    /// Whether or not there is a field in the compound type that replaces the body of the output
    /// record.
    pub replaces_body: bool,
    /// Whether or not there is a field in the compound type that is promoted to the header's body.
    pub header_body: bool,
    /// Whether or not there are fields that are written to the attributes vector in the record.
    pub has_attr_fields: bool,
    /// Whether or not there are fields that are written to the slot vector in the record.
    pub has_slot_fields: bool,
    /// Whether or not there are fields tha are written as headers in the record.
    pub has_header_fields: bool,
}
