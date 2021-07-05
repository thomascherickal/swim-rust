// Copyright 2015-2021 SWIM.AI inc.
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

use crate::structural::model::enumeration::{EnumDef, EnumModel, SegregatedEnumModel};
use crate::structural::model::record::{SegregatedStructModel, StructDef, StructModel};
use crate::structural::model::StructLike;
use crate::structural::model::ValidateFrom;
use crate::structural::read::DeriveStructuralReadable;
use crate::structural::write::DeriveStructuralWritable;
use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::{Data, DeriveInput, Generics};
use utilities::algebra::Errors;

pub mod model;
pub mod read;
pub mod write;

pub fn build_derive_structural_writable(
    input: DeriveInput,
) -> Result<TokenStream, Errors<syn::Error>> {
    match &input.data {
        Data::Struct(ds) => {
            let def = StructDef::new(&input.ident, &input, &input.attrs, ds);
            struct_derive_structural_writable(def, &input.generics)
        }
        Data::Enum(de) => {
            let def = EnumDef::new(&input.ident, &input, &input.attrs, de);
            enum_derive_structural_writable(def, &input.generics)
        }
        _ => Err(Errors::of(syn::Error::new_spanned(
            input,
            "Union types are not supported.",
        ))),
    }
}

fn struct_derive_structural_writable<'a, Flds: StructLike>(
    input: StructDef<'a, Flds>,
    generics: &'a Generics,
) -> Result<TokenStream, Errors<syn::Error>> {
    let model = StructModel::validate(input).into_result()?;
    let segregated = SegregatedStructModel::from(&model);
    let derive = DeriveStructuralWritable(segregated, generics);
    Ok(derive.into_token_stream())
}

fn enum_derive_structural_writable<'a>(
    input: EnumDef<'a>,
    generics: &'a Generics,
) -> Result<TokenStream, Errors<syn::Error>> {
    let model = EnumModel::validate(input).into_result()?;
    let segregated = SegregatedEnumModel::from(&model);
    let derive = DeriveStructuralWritable(segregated, generics);
    Ok(derive.into_token_stream())
}

pub fn build_derive_structural_readable(
    input: DeriveInput,
) -> Result<TokenStream, Errors<syn::Error>> {
    match &input.data {
        Data::Struct(ds) => {
            let def = StructDef::new(&input.ident, &input, &input.attrs, ds);
            struct_derive_structural_readable(def, &input.generics)
        }
        Data::Enum(de) => {
            let def = EnumDef::new(&input.ident, &input, &input.attrs, de);
            enum_derive_structural_readable(def, &input.generics)
        }
        _ => Err(Errors::of(syn::Error::new_spanned(
            input,
            "Union types are not supported.",
        ))),
    }
}

fn struct_derive_structural_readable<Flds: StructLike>(
    input: StructDef<'_, Flds>,
    generics: &Generics,
) -> Result<TokenStream, Errors<syn::Error>> {
    let model = StructModel::validate(input).into_result()?;
    let segregated = SegregatedStructModel::from(&model);
    let derive = DeriveStructuralReadable(segregated, generics);
    Ok(derive.into_token_stream())
}

fn enum_derive_structural_readable(
    input: EnumDef<'_>,
    generics: &Generics,
) -> Result<TokenStream, Errors<syn::Error>> {
    let model = EnumModel::validate(input).into_result()?;
    let segregated = SegregatedEnumModel::from(&model);
    let derive = DeriveStructuralReadable(segregated, generics);
    Ok(derive.into_token_stream())
}
