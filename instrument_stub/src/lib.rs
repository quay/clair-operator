use proc_macro::TokenStream;

/// Instrument is a no-op macro to be used instead of `tracing::instrument`.
#[proc_macro_attribute]
pub fn instrument(_args: TokenStream, inner: TokenStream) -> TokenStream {
    inner
}
