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

use crate::form::form_parser::FormDescriptor;
use crate::parser::{EnumVariant, FieldKind, FieldManifest, FormField, StructRepr, TypeContents};
use macro_helpers::{deconstruct_type, CompoundTypeKind, Label};
use proc_macro2::Ident;
use syn::export::TokenStream2;

use crate::parser::{
    EnumVariant, Field, FieldKind, FieldManifest, FormDescriptor, StructRepr, TypeContents,
};

pub fn to_value(
    type_contents: TypeContents<FormDescriptor, FormField<'_>>,
    structure_name: &Ident,
    descriptor: FormDescriptor,
    fn_factory: fn(&Ident) -> TokenStream2,
    requires_deref: bool,
) -> TokenStream2 {
    match type_contents {
        TypeContents::Struct(StructRepr {
            compound_type,
            fields,
            manifest,
            ..
        }) => build_struct_as_value(
            descriptor,
            manifest,
            &structure_name,
            &compound_type,
            &fields,
            fn_factory,
            requires_deref,
        ),
        TypeContents::Enum(variants) => {
            let arms = variants
                .into_iter()
                .fold(Vec::new(), |mut as_value_arms, variant| {
                    let EnumVariant {
                        name,
                        compound_type,
                        fields,
                        manifest,
                    } = variant;

                    let as_value = build_variant_as_value(
                        descriptor,
                        manifest,
                        &name,
                        &compound_type,
                        &fields,
                        fn_factory,
                        requires_deref,
                    );

                    as_value_arms.push(as_value);
                    as_value_arms
                });

            if requires_deref {
                quote! {
                    match *self {
                        #(#arms)*
                    }
                }
            } else {
                quote! {
                    match self {
                        #(#arms)*
                    }
                }
            }
        }
    }
}

fn build_struct_as_value(
    mut descriptor: FormDescriptor,
    mut manifest: FieldManifest,
    structure_name: &Ident,
    compound_type: &CompoundTypeKind,
    fields: &[FormField],
    fn_factory: fn(&Ident) -> TokenStream2,
    requires_deref: bool,
) -> TokenStream2 {
    let structure_name_str = descriptor.name.tag_ident.to_string();
    let (headers, attributes, items) =
        compute_as_value(&fields, &mut descriptor, &mut manifest, fn_factory);
    let field_names: Vec<_> = fields.iter().map(|f| &f.name).collect();
    let self_deconstruction = deconstruct_type(compound_type, &field_names, requires_deref);

    quote! {
        let #structure_name #self_deconstruction = self;
        let mut attrs = vec![swim_common::model::Attr::of((#structure_name_str #headers)), #attributes];
        swim_common::model::Value::Record(attrs, #items)
    }
}

fn build_variant_as_value(
    mut descriptor: FormDescriptor,
    mut manifest: FieldManifest,
    variant_name: &Label,
    compound_type: &CompoundTypeKind,
    fields: &[FormField],
    fn_factory: fn(&Ident) -> TokenStream2,
    requires_deref: bool,
    structure_name: &Ident,
) -> TokenStream2 {
    let variant_name_str = variant_name.to_string();
    let (headers, attributes, items) =
        compute_as_value(&fields, &mut descriptor, &mut manifest, fn_factory);
    let structure_name = &descriptor.name.original_ident;
    let field_names: Vec<_> = fields.iter().map(|f| &f.name).collect();
    let self_deconstruction = deconstruct_type(compound_type, &field_names, requires_deref);

    quote! {
        #structure_name::#variant_name #self_deconstruction => {
            let mut attrs = vec![swim_common::model::Attr::of((#variant_name_str #headers)), #attributes];
            swim_common::model::Value::Record(attrs, #items)
        },
    }
}

fn compute_as_value(
    fields: &[FormField],
    descriptor: &mut FormDescriptor,
    manifest: &mut FieldManifest,
    fn_factory: fn(&Ident) -> TokenStream2,
) -> (TokenStream2, TokenStream2, TokenStream2) {
    let (mut headers, mut items, attributes) = fields
        .iter()
        .fold((TokenStream2::new(), TokenStream2::new(), TokenStream2::new()), |(mut headers, mut items, mut attributes), f| {
            let name = &f.name;

            match &f.kind {
                FieldKind::Skip => {}
                FieldKind::Slot if !manifest.replaces_body => {
                    match name {
                        un @ Label::Anonymous(_) => {
                            let ident = un.as_ident();
                            let func = fn_factory(&ident);
                            items = quote!(#items swim_common::model::Item::ValueItem(#func),);
                        }
                        fi => {
                            let name_str = fi.to_string();
                            let func = fn_factory(&fi.as_ident());
                            items = quote!(#items swim_common::model::Item::Slot(swim_common::model::Value::Text(#name_str.to_string()), #func),);
                        }
                    }
                }
                FieldKind::Attr => {
                    let name_str = name.to_string();
                    let func = fn_factory(&name.as_ident());

                    attributes = quote!(#attributes swim_common::model::Attr::of((#name_str.to_string(), #func)),);
                }
                FieldKind::Body => {
                    descriptor.body_replaced = true;
                    let ident = f.name.as_ident();
                    let func = fn_factory(&ident);

                    items = quote!({
                        match #func {
                            swim_common::model::Value::Record(_attrs, items) => items,
                            v => vec![swim_common::model::Item::ValueItem(v)]
                        }
                    });
                }
                FieldKind::HeaderBody => {
                    let func = fn_factory(&name.as_ident());

                    if manifest.has_header_fields {
                        headers = quote!(#headers swim_common::model::Item::ValueItem(#func),);
                    } else {
                        headers = quote!(, #func);
                    }
                }
                _ => {
                    match name {
                        un @ Label::Anonymous(_) => {
                            let ident = un.as_ident();
                            let func = fn_factory(&ident);

                            headers = quote!(#headers swim_common::model::Item::ValueItem(#func),);
                        }
                        fi => {
                            let name_str = fi.to_string();
                            let func = fn_factory(&fi.as_ident());

                            headers = quote!(#headers swim_common::model::Item::Slot(swim_common::model::Value::Text(#name_str.to_string()), #func),);
                        }
                    }
                }
            }

            (headers, items, attributes)
        });

    if manifest.has_header_fields || manifest.replaces_body {
        headers = quote!(, swim_common::model::Value::Record(Vec::new(), vec![#headers]));
    }

    if !descriptor.has_body_replaced() {
        items = quote!(vec![#items]);
    }

    (headers, attributes, items)
}
