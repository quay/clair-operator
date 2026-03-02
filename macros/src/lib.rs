use proc_macro::TokenStream;

use proc_macro2::Span;
use quote::quote;
use syn::{
    DeriveInput, Error, Ident, Token, Type,
    parse::{Parse, ParseStream},
    parse_macro_input, parse_str,
    punctuated::{self, Punctuated},
};

/// Instrument is a no-op macro to be used instead of `tracing::instrument`.
#[proc_macro_attribute]
pub fn instrument(_args: TokenStream, inner: TokenStream) -> TokenStream {
    inner
}

/// Condition_reason implements [`controller::condition::Reason`] and [`PartialEq<String>`].
#[proc_macro_derive(ConditionReason)]
pub fn condition_reason(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);

    let ident = input.ident;
    let output = quote! {
        impl crate::condition::Reason for #ident {}

        impl PartialEq<String> for #ident {
            fn eq(&self, other: &String) -> bool {
                self.to_string().eq(other)
            }
        }
    };
    TokenStream::from(output)
}

/// Condition_types creates [`controller::condition::ConditionTypeFor`] implementations for the
/// listed types.
///
/// Only a limited set of known types are allowed.
#[proc_macro]
pub fn condition_types(items: TokenStream) -> TokenStream {
    let args = parse_macro_input!(items as ConditionTypesArgs);

    let expanded = args
        .into_iter()
        .map(|e| {
            let name = e.to_string();
            match name.as_str() {
                "ConfigMap" | "Secret" | "Service" => {
                    let ty: Type = parse_str(&format!("::k8s_openapi::api::core::v1::{e}"))
                        .expect("valid type");
                    Ok((ty, e))
                }
                "Deployment" => {
                    let ty = parse_str(&format!("::k8s_openapi::api::apps::v1::{e}"))
                        .expect("valid type");
                    Ok((ty, e))
                }
                "HorizontalPodAutoscaler" => {
                    let ty = parse_str(&format!("::k8s_openapi::api::autoscaling::v2::{e}"))
                        .expect("valid type");
                    Ok((ty, e))
                }
                "Indexer" | "Matcher" | "Notifier" => {
                    let ty = parse_str(&format!("::api::v1alpha1::{e}")).expect("valid type");
                    Ok((ty, e))
                }
                _ => Err(
                    Error::new(Span::call_site(), format!("unknown argument: {e}"))
                        .into_compile_error(),
                ),
            }
            .map(|(id, e)| {
                let ty: Type =
                    parse_str(&format!("crate::condition::Type::{e}Created")).expect("valid type");
                quote! {
                    impl crate::condition::ConditionTypeFor for #id {
                        const CONDITION_TYPE: crate::condition::Type = #ty;
                    }
                }
            })
        })
        .map(|r| match r {
            Ok(s) => s,
            Err(s) => s,
        })
        .map(TokenStream::from);

    TokenStream::from_iter(expanded)
}

struct ConditionTypesArgs(Punctuated<Ident, Token![,]>);

impl Parse for ConditionTypesArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let args = Punctuated::parse_separated_nonempty(input)?;
        Ok(Self(args))
    }
}

impl IntoIterator for ConditionTypesArgs {
    type Item = Ident;
    type IntoIter = punctuated::IntoIter<Ident>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
