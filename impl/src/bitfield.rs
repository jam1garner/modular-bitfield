use crate::{
    bitfield_attr::AttributeArgs,
    config::{
        Config,
        ReprKind,
    },
    errors::CombineError,
    field_config::{
        FieldConfig,
        SkipWhich,
    },
};
use core::convert::TryFrom;
use proc_macro2::TokenStream as TokenStream2;
use quote::{
    format_ident,
    quote,
    quote_spanned,
};
use std::collections::HashMap;
use syn::{
    self,
    parse::Result,
    punctuated::Punctuated,
    spanned::Spanned as _,
    Token,
};

/// Compactly stores all shared and useful information about a single `#[bitfield]` field.
pub struct FieldInfo<'a> {
    /// The index of the field.
    pub index: usize,
    /// The actual field.
    pub field: &'a syn::Field,
    /// The configuration of the field.
    pub config: FieldConfig,
}

impl<'a> FieldInfo<'a> {
    /// Creates a new field info.
    fn new(id: usize, field: &'a syn::Field, config: FieldConfig) -> Self {
        Self {
            index: id,
            field,
            config,
        }
    }

    /// Returns the ident fragment for this field.
    fn ident_frag(&self) -> &dyn quote::IdentFragment {
        match &self.field.ident {
            Some(ident) => ident,
            None => &self.index,
        }
    }

    /// Returns the field's identifier as `String`.
    fn name(&self) -> String {
        Self::ident_as_string(self.field, self.index)
    }

    /// Returns the field's identifier at the given index as `String`.
    fn ident_as_string(field: &'a syn::Field, index: usize) -> String {
        field
            .ident
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("{}", index))
    }
}

/// Analyzes the given token stream for `#[bitfield]` properties and expands code if valid.
pub fn analyse_and_expand(args: TokenStream2, input: TokenStream2) -> TokenStream2 {
    match analyse_and_expand_or_error(args, input) {
        Ok(output) => output,
        Err(err) => err.to_compile_error(),
    }
}

/// Analyzes the given token stream for `#[bitfield]` properties and expands code if valid.
///
/// # Errors
///
/// If the given token stream does not yield a valid `#[bitfield]` specifier.
fn analyse_and_expand_or_error(
    args: TokenStream2,
    input: TokenStream2,
) -> Result<TokenStream2> {
    let input = syn::parse::<syn::ItemStruct>(input.into())?;
    let attrs = syn::parse::<AttributeArgs>(args.into())?;
    let mut config = Config::try_from(attrs)?;
    let bitfield = BitfieldStruct::try_from((&mut config, input))?;
    Ok(bitfield.expand(&config))
}

/// Type used to guide analysis and expansion of `#[bitfield]` structs.
struct BitfieldStruct {
    /// The input `struct` item.
    item_struct: syn::ItemStruct,
}

impl TryFrom<(&mut Config, syn::ItemStruct)> for BitfieldStruct {
    type Error = syn::Error;

    fn try_from((config, item_struct): (&mut Config, syn::ItemStruct)) -> Result<Self> {
        Self::ensure_has_fields(&item_struct)?;
        Self::ensure_no_generics(&item_struct)?;
        Self::extract_attributes(&item_struct.attrs, config)?;
        Self::analyse_config_for_fields(&item_struct, config)?;
        Ok(Self { item_struct })
    }
}

impl BitfieldStruct {
    /// Returns an error if the input struct does not have any fields.
    fn ensure_has_fields(item_struct: &syn::ItemStruct) -> Result<()> {
        if let unit @ syn::Fields::Unit = &item_struct.fields {
            return Err(format_err_spanned!(
                unit,
                "encountered invalid bitfield struct without fields"
            ))
        }
        Ok(())
    }

    /// Returns an error if the input struct is generic.
    fn ensure_no_generics(item_struct: &syn::ItemStruct) -> Result<()> {
        if !item_struct.generics.params.is_empty() {
            return Err(format_err_spanned!(
                item_struct,
                "encountered invalid generic bitfield struct"
            ))
        }
        Ok(())
    }

    /// Extracts the `#[repr(uN)]` annotations from the given `#[bitfield]` struct.
    fn extract_repr_attribute(attr: &syn::Attribute, config: &mut Config) -> Result<()> {
        let path = &attr.path;
        let args = &attr.tokens;
        let meta: syn::MetaList = syn::parse2::<_>(quote! { #path #args })?;
        let mut retained_reprs = vec![];
        for nested_meta in meta.nested {
            let meta_span = nested_meta.span();
            match nested_meta {
                syn::NestedMeta::Meta(syn::Meta::Path(path)) => {
                    let repr_kind = if path.is_ident("u8") {
                        Some(ReprKind::U8)
                    } else if path.is_ident("u16") {
                        Some(ReprKind::U16)
                    } else if path.is_ident("u32") {
                        Some(ReprKind::U32)
                    } else if path.is_ident("u64") {
                        Some(ReprKind::U64)
                    } else if path.is_ident("u128") {
                        Some(ReprKind::U128)
                    } else {
                        // If other repr such as `transparent` or `C` have been found we
                        // are going to re-expand them into a new `#[repr(..)]` that is
                        // ignored by the rest of this macro.
                        retained_reprs.push(syn::NestedMeta::Meta(syn::Meta::Path(path)));
                        None
                    };
                    if let Some(repr_kind) = repr_kind {
                        config.repr(repr_kind, meta_span)?;
                    }
                }
                unknown => retained_reprs.push(unknown),
            }
        }
        if !retained_reprs.is_empty() {
            // We only push back another re-generated `#[repr(..)]` if its contents
            // contained some non-bitfield representations and thus is not empty.
            let retained_reprs_tokens = quote! {
                ( #( #retained_reprs ),* )
            };
            config.push_retained_attribute(syn::Attribute {
                pound_token: attr.pound_token,
                style: attr.style,
                bracket_token: attr.bracket_token,
                path: attr.path.clone(),
                tokens: retained_reprs_tokens,
            });
        }
        Ok(())
    }

    /// Extracts the `#[derive(Debug)]` annotations from the given `#[bitfield]` struct.
    fn extract_derive_debug_attribute(
        attr: &syn::Attribute,
        config: &mut Config,
    ) -> Result<()> {
        let path = &attr.path;
        let args = &attr.tokens;
        let meta: syn::MetaList = syn::parse2::<_>(quote! { #path #args })?;
        let mut retained_derives = vec![];
        for nested_meta in meta.nested {
            let meta_span = nested_meta.span();
            match nested_meta {
                syn::NestedMeta::Meta(syn::Meta::Path(path)) => {
                    if path.is_ident("Debug") {
                        config.derive_debug(true, meta_span)?;
                    } else {
                        // Other derives are going to be re-expanded them into a new
                        // `#[derive(..)]` that is ignored by the rest of this macro.
                        retained_derives
                            .push(syn::NestedMeta::Meta(syn::Meta::Path(path)));
                    };
                }
                unknown => retained_derives.push(unknown),
            }
        }
        if !retained_derives.is_empty() {
            // We only push back another re-generated `#[derive(..)]` if its contents
            // contain some remaining derives and thus is not empty.
            let retained_derives_tokens = quote! {
                ( #( #retained_derives ),* )
            };
            config.push_retained_attribute(syn::Attribute {
                pound_token: attr.pound_token,
                style: attr.style,
                bracket_token: attr.bracket_token,
                path: attr.path.clone(),
                tokens: retained_derives_tokens,
            });
        }
        Ok(())
    }

    /// Analyses and extracts the `#[repr(uN)]` or other annotations from the given struct.
    fn extract_attributes(
        attributes: &[syn::Attribute],
        config: &mut Config,
    ) -> Result<()> {
        for attr in attributes {
            if attr.path.is_ident("repr") {
                Self::extract_repr_attribute(attr, config)?;
            } else if attr.path.is_ident("derive") {
                Self::extract_derive_debug_attribute(attr, config)?;
            } else {
                config.push_retained_attribute(attr.clone());
            }
        }
        Ok(())
    }

    /// Returns an iterator over the names of the fields.
    ///
    /// If a field has no name it is replaced by its field number.
    fn fields(
        item_struct: &syn::ItemStruct,
    ) -> impl Iterator<Item = (usize, &syn::Field)> {
        item_struct
            .fields
            .iter()
            .enumerate()
            .map(|(n, field)| (n, field))
    }

    /// Returns an iterator over the names of the fields.
    ///
    /// If a field has no name it is replaced by its field number.
    fn field_infos<'a, 'b: 'a>(
        &'a self,
        config: &'b Config,
    ) -> impl Iterator<Item = FieldInfo<'a>> {
        Self::fields(&self.item_struct).map(move |(n, field)| {
            let field_config = config
                .field_configs
                .get(&n)
                .map(|config| &config.value)
                .cloned()
                .unwrap_or_default();
            FieldInfo::new(n, field, field_config)
        })
    }

    /// Analyses and extracts the configuration for all bitfield fields.
    fn analyse_config_for_fields(
        item_struct: &syn::ItemStruct,
        config: &mut Config,
    ) -> Result<()> {
        for (index, field) in Self::fields(item_struct) {
            let span = field.span();
            let field_config = Self::extract_field_config(field)?;
            config.field_config(index, span, field_config)?;
        }
        Ok(())
    }

    /// Extracts the `#[bits = N]` and `#[skip(..)]` attributes for a given field.
    fn extract_field_config(field: &syn::Field) -> Result<FieldConfig> {
        let mut config = FieldConfig::default();
        for attr in &field.attrs {
            if attr.path.is_ident("bits") {
                let path = &attr.path;
                let args = &attr.tokens;
                let name_value: syn::MetaNameValue =
                    syn::parse2::<_>(quote! { #path #args })?;
                let span = name_value.span();
                match name_value.lit {
                    syn::Lit::Int(lit_int) => {
                        config.bits(lit_int.base10_parse::<usize>()?, span)?;
                    }
                    _ => {
                        return Err(format_err!(
                            span,
                            "encountered invalid value type for #[bits = N]"
                        ))
                    }
                }
            } else if attr.path.is_ident("skip") {
                let path = &attr.path;
                let args = &attr.tokens;
                let meta: syn::Meta = syn::parse2::<_>(quote! { #path #args })?;
                let span = meta.span();
                match meta {
                    syn::Meta::Path(path) => {
                        assert!(path.is_ident("skip"));
                        config.skip(SkipWhich::All, span)?;
                    }
                    syn::Meta::List(meta_list) => {
                        let mut which = HashMap::new();
                        for nested_meta in &meta_list.nested {
                            match nested_meta {
                                syn::NestedMeta::Meta(syn::Meta::Path(path)) => {
                                    if path.is_ident("getters") {
                                        if let Some(previous) =
                                            which.insert(SkipWhich::Getters, span)
                                        {
                                            return Err(format_err!(
                                                span,
                                                "encountered duplicate #[skip(getters)]"
                                            )
                                            .into_combine(format_err!(
                                                previous,
                                                "previous found here"
                                            )))
                                        }
                                    } else if path.is_ident("setters") {
                                        if let Some(previous) =
                                            which.insert(SkipWhich::Setters, span)
                                        {
                                            return Err(format_err!(
                                                span,
                                                "encountered duplicate #[skip(setters)]"
                                            )
                                            .into_combine(format_err!(
                                                previous,
                                                "previous found here"
                                            )))
                                        }
                                    } else {
                                        return Err(format_err!(
                                            nested_meta.span(),
                                            "encountered unknown or unsupported #[skip(..)] specifier"
                                        ))
                                    }
                                }
                                _ => return Err(format_err!(span, "encountered invalid #[skip] field attribute argument"))
                            }
                        }
                        if which.is_empty()
                            || which.contains_key(&SkipWhich::Getters)
                                && which.contains_key(&SkipWhich::Setters)
                        {
                            config.skip(SkipWhich::All, span)?;
                        } else if which.contains_key(&SkipWhich::Getters) {
                            config.skip(SkipWhich::Getters, span)?;
                        } else if which.contains_key(&SkipWhich::Setters) {
                            config.skip(SkipWhich::Setters, span)?;
                        }
                    }
                    _ => {
                        return Err(format_err!(
                            span,
                            "encountered invalid format for #[skip] field attribute"
                        ))
                    }
                }
            } else {
                config.retain_attr(attr.clone());
            }
        }
        Ok(config)
    }

    /// Expands the given `#[bitfield]` struct into an actual bitfield definition.
    pub fn expand(&self, config: &Config) -> TokenStream2 {
        let span = self.item_struct.span();
        let check_filled = self.generate_check_for_filled(config);
        let struct_definition = self.generate_struct(config);
        let constructor_definition = self.generate_constructor();
        let specifier_impl = self.generate_specifier_impl(config);

        let byte_conversion_impls = self.expand_byte_conversion_impls(config);
        let getters_and_setters = self.expand_getters_and_setters(config);
        let bytes_check = self.expand_optional_bytes_check(config);
        let repr_impls_and_checks = self.expand_repr_from_impls_and_checks(config);
        let debug_impl = self.generate_debug_impl(config);

        quote_spanned!(span=>
            #struct_definition
            #check_filled
            #constructor_definition
            #byte_conversion_impls
            #getters_and_setters
            #specifier_impl
            #bytes_check
            #repr_impls_and_checks
            #debug_impl
        )
    }

    /// Expands to the `Specifier` impl for the `#[bitfield]` struct if `specifier = true`.
    ///
    /// Otherwise returns `None`.
    pub fn generate_specifier_impl(&self, config: &Config) -> Option<TokenStream2> {
        if !config.specifier_enabled() {
            return None
        }
        let span = self.item_struct.span();
        let ident = &self.item_struct.ident;
        let bits = self.generate_bitfield_size();
        let next_divisible_by_8 = Self::next_divisible_by_8(&bits);
        Some(quote_spanned!(span =>
            #[allow(clippy::identity_op)]
            const _: () = {
                impl ::modular_bitfield::private::checks::CheckSpecifierHasAtMost128Bits for #ident {
                    type CheckType = [(); (#bits <= 128) as usize];
                }
            };

            #[allow(clippy::identity_op)]
            impl ::modular_bitfield::Specifier for #ident {
                const BITS: usize = #bits;

                type Bytes = <[(); if #bits > 128 { 128 } else #bits] as ::modular_bitfield::private::SpecifierBytes>::Bytes;
                type InOut = Self;

                #[inline]
                fn into_bytes(
                    value: Self::InOut,
                ) -> ::core::result::Result<Self::Bytes, ::modular_bitfield::error::OutOfBounds> {
                    ::core::result::Result::Ok(
                        <[(); #next_divisible_by_8] as ::modular_bitfield::private::ArrayBytesConversion>::array_into_bytes(value.bytes)
                    )
                }

                #[inline]
                fn from_bytes(
                    bytes: Self::Bytes,
                ) -> ::core::result::Result<Self::InOut, ::modular_bitfield::error::InvalidBitPattern<Self::Bytes>>
                {
                    let __bf_max_value: Self::Bytes = (0x01 as Self::Bytes).checked_shl(Self::BITS as u32).unwrap_or(<Self::Bytes>::MAX);
                    if bytes > __bf_max_value {
                        return ::core::result::Result::Err(::modular_bitfield::error::InvalidBitPattern::new(bytes))
                    }
                    let __bf_bytes = bytes.to_le_bytes();
                    ::core::result::Result::Ok(Self {
                        bytes: <[(); #next_divisible_by_8] as ::modular_bitfield::private::ArrayBytesConversion>::bytes_into_array(bytes)
                    })
                }
            }
        ))
    }

    /// Generates the core::fmt::Debug impl if `#[derive(Debug)]` is included.
    pub fn generate_debug_impl(&self, config: &Config) -> Option<TokenStream2> {
        config.derive_debug.as_ref()?;
        let span = self.item_struct.span();
        let ident = &self.item_struct.ident;
        let fields = self.field_infos(config).map(|info| {
            let FieldInfo {
                index: _,
                field,
                config,
            } = &info;
            if config.skip_getters() {
                return None
            }
            let field_span = field.span();
            let field_name = info.name();
            let field_ident = info.ident_frag();
            let field_getter = field
                .ident
                .as_ref()
                .map(|_| format_ident!("{}_or_err", field_ident))
                .unwrap_or_else(|| format_ident!("get_{}_or_err", field_ident));
            Some(quote_spanned!(field_span=>
                .field(
                    #field_name,
                    self.#field_getter()
                        .as_ref()
                        .map(|__bf_field| __bf_field as &dyn ::core::fmt::Debug)
                        .unwrap_or_else(|__bf_err| __bf_err as &dyn ::core::fmt::Debug)
                )
            ))
        });
        Some(quote_spanned!(span=>
            impl ::core::fmt::Debug for #ident {
                fn fmt(&self, __bf_f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                    __bf_f.debug_struct(::core::stringify!(#ident))
                        #( #fields )*
                        .finish()
                }
            }
        ))
    }

    /// Generates the expression denoting the sum of all field bit specifier sizes.
    ///
    /// # Example
    ///
    /// For the following struct:
    ///
    /// ```
    /// # use modular_bitfield::prelude::*;
    /// #[bitfield]
    /// pub struct Color {
    ///     r: B8,
    ///     g: B8,
    ///     b: B8,
    ///     a: bool,
    ///     rest: B7,
    /// }
    /// ```
    ///
    /// We generate the following tokens:
    ///
    /// ```
    /// # use modular_bitfield::prelude::*;
    /// {
    ///     0usize +
    ///     <B8 as ::modular_bitfield::Specifier>::BITS +
    ///     <B8 as ::modular_bitfield::Specifier>::BITS +
    ///     <B8 as ::modular_bitfield::Specifier>::BITS +
    ///     <bool as ::modular_bitfield::Specifier>::BITS +
    ///     <B7 as ::modular_bitfield::Specifier>::BITS
    /// }
    /// # ;
    /// ```
    ///
    /// Which is a compile time evaluatable expression.
    fn generate_bitfield_size(&self) -> TokenStream2 {
        let span = self.item_struct.span();
        let sum = self
            .item_struct
            .fields
            .iter()
            .map(|field| {
                let span = field.span();
                let ty = &field.ty;
                quote_spanned!(span=>
                    <#ty as ::modular_bitfield::Specifier>::BITS
                )
            })
            .fold(quote_spanned!(span=> 0usize), |lhs, rhs| {
                quote_spanned!(span =>
                    #lhs + #rhs
                )
            });
        quote_spanned!(span=>
            { #sum }
        )
    }

    /// Generate check for either of the following two cases:
    ///
    /// - `filled = true`: Check if the total number of required bits is a multiple of 8.
    /// - `filled = false`: Check if the total number of required bits is NOT a multiple of 8.
    fn generate_check_for_filled(&self, config: &Config) -> TokenStream2 {
        let span = self.item_struct.span();
        let ident = &self.item_struct.ident;
        let size = self.generate_bitfield_size();
        let check_ident = match config.filled_enabled() {
            true => quote_spanned!(span => CheckTotalSizeMultipleOf8),
            false => quote_spanned!(span => CheckTotalSizeIsNotMultipleOf8),
        };
        quote_spanned!(span=>
            #[allow(clippy::identity_op)]
            const _: () = {
                impl ::modular_bitfield::private::checks::#check_ident for #ident {
                    type Size = ::modular_bitfield::private::checks::TotalSize<[(); #size % 8usize]>;
                }
            };
        )
    }

    /// Returns a token stream representing the next greater value divisible by 8.
    fn next_divisible_by_8(value: &TokenStream2) -> TokenStream2 {
        let span = value.span();
        quote_spanned!(span=> {
            (((#value - 1) / 8) + 1) * 8
        })
    }

    /// Generates the actual item struct definition for the `#[bitfield]`.
    ///
    /// Internally it only contains a byte array equal to the minimum required
    /// amount of bytes to compactly store the information of all its bit fields.
    fn generate_struct(&self, config: &Config) -> TokenStream2 {
        let span = self.item_struct.span();
        let attrs = &config.retained_attributes;
        let vis = &self.item_struct.vis;
        let ident = &self.item_struct.ident;
        let size = self.generate_bitfield_size();
        let next_divisible_by_8 = Self::next_divisible_by_8(&size);
        quote_spanned!(span=>
            #( #attrs )*
            #[allow(clippy::identity_op)]
            #vis struct #ident
            {
                bytes: [::core::primitive::u8; #next_divisible_by_8 / 8usize],
            }
        )
    }

    /// Generates the constructor for the bitfield that initializes all bytes to zero.
    fn generate_constructor(&self) -> TokenStream2 {
        let span = self.item_struct.span();
        let ident = &self.item_struct.ident;
        let size = self.generate_bitfield_size();
        let next_divisible_by_8 = Self::next_divisible_by_8(&size);
        quote_spanned!(span=>
            impl #ident
            {
                /// Returns an instance with zero initialized data.
                #[allow(clippy::identity_op)]
                pub const fn new() -> Self {
                    Self {
                        bytes: [0u8; #next_divisible_by_8 / 8usize],
                    }
                }
            }
        )
    }

    /// Generates the compile-time assertion if the optional `byte` parameter has been set.
    fn expand_optional_bytes_check(&self, config: &Config) -> Option<TokenStream2> {
        let ident = &self.item_struct.ident;
        config.bytes.as_ref().map(|config| {
            let bytes = config.value;
            quote_spanned!(config.span=>
                const _: () = {
                    struct ExpectedBytes { __bf_unused: [::core::primitive::u8; #bytes] };

                    ::modular_bitfield::private::static_assertions::assert_eq_size!(
                        ExpectedBytes,
                        #ident
                    );
                };
            )
        })
    }

    /// Generates `From` impls for a `#[repr(uN)]` annotated #[bitfield] struct.
    fn expand_repr_from_impls_and_checks(&self, config: &Config) -> Option<TokenStream2> {
        let ident = &self.item_struct.ident;
        config.repr.as_ref().map(|repr| {
            let kind = &repr.value;
            let span = repr.span;
            let prim = match kind {
                ReprKind::U8 => quote! { ::core::primitive::u8 },
                ReprKind::U16 => quote! { ::core::primitive::u16 },
                ReprKind::U32 => quote! { ::core::primitive::u32 },
                ReprKind::U64 => quote! { ::core::primitive::u64 },
                ReprKind::U128 => quote! { ::core::primitive::u128 },
            };
            let actual_bits = self.generate_bitfield_size();
            let trait_check_ident = match kind {
                ReprKind::U8 => quote! { IsU8Compatible },
                ReprKind::U16 => quote! { IsU16Compatible },
                ReprKind::U32 => quote! { IsU32Compatible },
                ReprKind::U64 => quote! { IsU64Compatible },
                ReprKind::U128 => quote! { IsU128Compatible },
            };
            quote_spanned!(span=>
                impl ::core::convert::From<#prim> for #ident
                where
                    [(); #actual_bits]: ::modular_bitfield::private::#trait_check_ident,
                {
                    #[inline]
                    fn from(__bf_prim: #prim) -> Self {
                        Self { bytes: <#prim>::to_le_bytes(__bf_prim) }
                    }
                }

                impl ::core::convert::From<#ident> for #prim
                where
                    [(); #actual_bits]: ::modular_bitfield::private::#trait_check_ident,
                {
                    #[inline]
                    fn from(__bf_bitfield: #ident) -> Self {
                        <Self>::from_le_bytes(__bf_bitfield.bytes)
                    }
                }
            )
        })
    }

    /// Generates routines to allow conversion from and to bytes for the `#[bitfield]` struct.
    fn expand_byte_conversion_impls(&self, config: &Config) -> TokenStream2 {
        let span = self.item_struct.span();
        let ident = &self.item_struct.ident;
        let size = self.generate_bitfield_size();
        let next_divisible_by_8 = Self::next_divisible_by_8(&size);
        let from_bytes = match config.filled_enabled() {
            true => {
                quote_spanned!(span=>
                    /// Converts the given bytes directly into the bitfield struct.
                    #[inline]
                    #[allow(clippy::identity_op)]
                    pub const fn from_bytes(bytes: [::core::primitive::u8; #next_divisible_by_8 / 8usize]) -> Self {
                        Self { bytes }
                    }
                )
            }
            false => {
                quote_spanned!(span=>
                    /// Converts the given bytes directly into the bitfield struct.
                    ///
                    /// # Errors
                    ///
                    /// If the given bytes contain bits at positions that are undefined for `Self`.
                    #[inline]
                    #[allow(clippy::identity_op)]
                    pub fn from_bytes(
                        bytes: [::core::primitive::u8; #next_divisible_by_8 / 8usize]
                    ) -> ::core::result::Result<Self, ::modular_bitfield::error::OutOfBounds> {
                        if bytes[(#next_divisible_by_8 / 8usize) - 1] >= (0x01 << (8 - (#next_divisible_by_8 - #size))) {
                            return ::core::result::Result::Err(::modular_bitfield::error::OutOfBounds)
                        }
                        ::core::result::Result::Ok(Self { bytes })
                    }
                )
            }
        };
        quote_spanned!(span=>
            impl #ident {
                /// Returns the underlying bits.
                ///
                /// # Layout
                ///
                /// The returned byte array is layed out in the same way as described
                /// [here](https://docs.rs/modular-bitfield/#generated-structure).
                #[inline]
                #[allow(clippy::identity_op)]
                pub const fn into_bytes(self) -> [::core::primitive::u8; #next_divisible_by_8 / 8usize] {
                    self.bytes
                }

                #from_bytes
            }
        )
    }

    /// Generates code to check for the bit size arguments of bitfields.
    fn expand_bits_checks_for_field(&self, field_info: FieldInfo<'_>) -> TokenStream2 {
        let FieldInfo {
            index: _,
            field,
            config,
        } = field_info;
        let span = field.span();
        let bits_check = match &config.bits {
            Some(bits) => {
                let ty = &field.ty;
                let expected_bits = bits.value;
                let span = bits.span;
                Some(quote_spanned!(span =>
                    let _: ::modular_bitfield::private::checks::BitsCheck::<[(); #expected_bits]> =
                        ::modular_bitfield::private::checks::BitsCheck::<[(); #expected_bits]>{
                            arr: [(); <#ty as ::modular_bitfield::Specifier>::BITS]
                        };
                ))
            }
            None => None,
        };
        quote_spanned!(span=>
            const _: () = {
                #bits_check
            };
        )
    }

    fn expand_getters_for_field(
        &self,
        offset: &Punctuated<syn::Expr, syn::Token![+]>,
        info: &FieldInfo<'_>,
    ) -> Option<TokenStream2> {
        let FieldInfo {
            index: _,
            field,
            config,
        } = &info;
        if config.skip_getters() {
            return None
        }
        let struct_ident = &self.item_struct.ident;
        let span = field.span();
        let ident = info.ident_frag();
        let name = info.name();

        let retained_attrs = &config.retained_attrs;
        let get_ident = field
            .ident
            .as_ref()
            .cloned()
            .unwrap_or_else(|| format_ident!("get_{}", ident));
        let get_checked_ident = field
            .ident
            .as_ref()
            .map(|_| format_ident!("{}_or_err", ident))
            .unwrap_or_else(|| format_ident!("get_{}_or_err", ident));
        let ty = &field.ty;
        let vis = &field.vis;
        let get_assert_msg = format!(
            "value contains invalid bit pattern for field {}.{}",
            struct_ident, name
        );

        let getter_docs = format!("Returns the value of {}.", name);
        let checked_getter_docs = format!(
            "Returns the value of {}.\n\n\
             #Errors\n\n\
             If the returned value contains an invalid bit pattern for {}.",
            name, name,
        );
        let getters = quote_spanned!(span=>
            #[doc = #getter_docs]
            #[inline]
            #( #retained_attrs )*
            #vis fn #get_ident(&self) -> <#ty as ::modular_bitfield::Specifier>::InOut {
                self.#get_checked_ident().expect(#get_assert_msg)
            }

            #[doc = #checked_getter_docs]
            #[inline]
            #[allow(dead_code)]
            #( #retained_attrs )*
            #vis fn #get_checked_ident(
                &self,
            ) -> ::core::result::Result<
                <#ty as ::modular_bitfield::Specifier>::InOut,
                ::modular_bitfield::error::InvalidBitPattern<<#ty as ::modular_bitfield::Specifier>::Bytes>
            > {
                let __bf_read: <#ty as ::modular_bitfield::Specifier>::Bytes = {
                    ::modular_bitfield::private::read_specifier::<#ty>(&self.bytes[..], #offset)
                };
                <#ty as ::modular_bitfield::Specifier>::from_bytes(__bf_read)
            }
        );
        Some(getters)
    }

    fn expand_setters_for_field(
        &self,
        offset: &Punctuated<syn::Expr, syn::Token![+]>,
        info: &FieldInfo<'_>,
    ) -> Option<TokenStream2> {
        let FieldInfo {
            index: _,
            field,
            config,
        } = &info;
        if config.skip_setters() {
            return None
        }
        let struct_ident = &self.item_struct.ident;
        let span = field.span();
        let retained_attrs = &config.retained_attrs;

        let ident = info.ident_frag();
        let name = info.name();
        let ty = &field.ty;
        let vis = &field.vis;

        let set_ident = format_ident!("set_{}", ident);
        let set_checked_ident = format_ident!("set_{}_checked", ident);
        let with_ident = format_ident!("with_{}", ident);
        let with_checked_ident = format_ident!("with_{}_checked", ident);

        let set_assert_msg =
            format!("value out of bounds for field {}.{}", struct_ident, name);
        let setter_docs = format!(
            "Sets the value of {} to the given value.\n\n\
             #Panics\n\n\
             If the given value is out of bounds for {}.",
            name, name,
        );
        let checked_setter_docs = format!(
            "Sets the value of {} to the given value.\n\n\
             #Errors\n\n\
             If the given value is out of bounds for {}.",
            name, name,
        );
        let with_docs = format!(
            "Returns a copy of the bitfield with the value of {} \
             set to the given value.\n\n\
             #Panics\n\n\
             If the given value is out of bounds for {}.",
            name, name,
        );
        let checked_with_docs = format!(
            "Returns a copy of the bitfield with the value of {} \
             set to the given value.\n\n\
             #Errors\n\n\
             If the given value is out of bounds for {}.",
            name, name,
        );
        let setters = quote_spanned!(span=>
            #[doc = #with_docs]
            #[inline]
            #[allow(dead_code)]
            #( #retained_attrs )*
            #vis fn #with_ident(
                mut self,
                new_val: <#ty as ::modular_bitfield::Specifier>::InOut
            ) -> Self {
                self.#set_ident(new_val);
                self
            }

            #[doc = #checked_with_docs]
            #[inline]
            #[allow(dead_code)]
            #( #retained_attrs )*
            #vis fn #with_checked_ident(
                mut self,
                new_val: <#ty as ::modular_bitfield::Specifier>::InOut,
            ) -> ::core::result::Result<Self, ::modular_bitfield::error::OutOfBounds> {
                self.#set_checked_ident(new_val)?;
                ::core::result::Result::Ok(self)
            }

            #[doc = #setter_docs]
            #[inline]
            #[allow(dead_code)]
            #( #retained_attrs )*
            #vis fn #set_ident(&mut self, new_val: <#ty as ::modular_bitfield::Specifier>::InOut) {
                self.#set_checked_ident(new_val).expect(#set_assert_msg)
            }

            #[doc = #checked_setter_docs]
            #[inline]
            #( #retained_attrs )*
            #vis fn #set_checked_ident(
                &mut self,
                new_val: <#ty as ::modular_bitfield::Specifier>::InOut
            ) -> ::core::result::Result<(), ::modular_bitfield::error::OutOfBounds> {
                let __bf_base_bits: ::core::primitive::usize = 8usize * ::core::mem::size_of::<<#ty as ::modular_bitfield::Specifier>::Bytes>();
                let __bf_max_value: <#ty as ::modular_bitfield::Specifier>::Bytes = {
                    !0 >> (__bf_base_bits - <#ty as ::modular_bitfield::Specifier>::BITS)
                };
                let __bf_spec_bits: ::core::primitive::usize = <#ty as ::modular_bitfield::Specifier>::BITS;
                let __bf_raw_val: <#ty as ::modular_bitfield::Specifier>::Bytes = {
                    <#ty as ::modular_bitfield::Specifier>::into_bytes(new_val)
                }?;
                // We compare base bits with spec bits to drop this condition
                // if there cannot be invalid inputs.
                if !(__bf_base_bits == __bf_spec_bits || __bf_raw_val <= __bf_max_value) {
                    return ::core::result::Result::Err(::modular_bitfield::error::OutOfBounds)
                }
                ::modular_bitfield::private::write_specifier::<#ty>(&mut self.bytes[..], #offset, __bf_raw_val);
                ::core::result::Result::Ok(())
            }
        );
        Some(setters)
    }

    fn expand_getters_and_setters_for_field(
        &self,
        offset: &mut Punctuated<syn::Expr, syn::Token![+]>,
        info: FieldInfo<'_>,
    ) -> Option<TokenStream2> {
        let FieldInfo {
            index: _,
            field,
            config,
        } = &info;
        if config.skip_getters_and_setters() {
            return None
        }
        let span = field.span();
        let ty = &field.ty;
        let getters = self.expand_getters_for_field(offset, &info);
        let setters = self.expand_setters_for_field(offset, &info);
        let getters_and_setters = quote_spanned!(span=>
            #getters
            #setters
        );
        offset.push(syn::parse_quote! { <#ty as ::modular_bitfield::Specifier>::BITS });
        Some(getters_and_setters)
    }

    fn expand_getters_and_setters(&self, config: &Config) -> TokenStream2 {
        let span = self.item_struct.span();
        let ident = &self.item_struct.ident;
        let mut offset = {
            let mut offset = Punctuated::<syn::Expr, Token![+]>::new();
            offset.push(syn::parse_quote! { 0usize });
            offset
        };
        let bits_checks = self
            .field_infos(config)
            .map(|field_info| self.expand_bits_checks_for_field(field_info));
        let setters_and_getters = self.field_infos(config).map(|field_info| {
            self.expand_getters_and_setters_for_field(&mut offset, field_info)
        });
        quote_spanned!(span=>
            const _: () = {
                #( #bits_checks )*
            };

            impl #ident {
                #( #setters_and_getters )*
            }
        )
    }
}
