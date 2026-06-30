use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote, ToTokens};
use syn::{parse_macro_input, FnArg, Ident, ItemFn, Pat, ReturnType, Type};

#[proc_macro_attribute]
pub fn fn_info(args: TokenStream, input: TokenStream) -> TokenStream {
    if !args.is_empty() {
        return quote! {
            compile_error!("fn_info derives metadata from the function shape and does not accept arguments");
        }
        .into();
    }

    let function = parse_macro_input!(input as ItemFn);
    expand_fn_info(function)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_fn_info(function: ItemFn) -> syn::Result<TokenStream2> {
    let fn_name = &function.sig.ident;
    let info_name = format_ident!("{fn_name}_info");
    let function_name = fn_name.to_string();

    let mut params = Vec::new();
    let mut extract_args = Vec::new();
    let mut call_args = Vec::new();

    for (index, arg) in function.sig.inputs.iter().enumerate() {
        let FnArg::Typed(arg) = arg else {
            return Err(syn::Error::new_spanned(
                arg,
                "fn_info does not support methods",
            ));
        };
        let Pat::Ident(pat) = arg.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &arg.pat,
                "fn_info only supports identifier parameters",
            ));
        };

        let ident = &pat.ident;
        let name = ident.to_string();
        let type_info = type_info_expr(&arg.ty)?;
        let extraction = extraction_expr(index, ident, &arg.ty)?;

        params.push(quote! {
            ParamInfo {
                name: #name,
                r#type: #type_info,
            }
        });
        extract_args.push(extraction);
        call_args.push(quote! { #ident });
    }

    let return_type = return_type_expr(&function.sig.output)?;
    let return_value = return_value_expr(fn_name, &call_args, &function.sig.output)?;

    Ok(quote! {
        #[allow(dead_code)]
        fn #info_name() -> FnInfo {
            FnInfo {
                name: #function_name,
                params: vec![#(#params),*],
                return_type: #return_type,
                function: Box::new(|params: &[Value]| {
                    #(#extract_args)*
                    #return_value
                }),
            }
        }

        #function
    })
}

fn type_info_expr(ty: &Type) -> syn::Result<TokenStream2> {
    json_type_info_expr(ty).ok_or_else(|| {
        syn::Error::new_spanned(
            ty,
            format!("unsupported fn_info parameter type `{}`", type_text(ty)),
        )
    })
}

fn json_type_info_expr(ty: &Type) -> Option<TokenStream2> {
    if is_value(ty) || is_value_ref(ty) || is_option_value_ref(ty) {
        Some(quote! { JsonTypeInfo::Any })
    } else if is_string(ty) {
        Some(quote! { JsonTypeInfo::String })
    } else if is_value_slice(ty) || is_value_vec_ref(ty) {
        Some(quote! { JsonTypeInfo::Array(JsonTypeInfo::Any.into()) })
    } else if is_option_usize(ty) {
        Some(quote! { JsonTypeInfo::OneOf(vec![JsonTypeInfo::Number, JsonTypeInfo::Null]) })
    } else {
        None
    }
}

fn extraction_expr(index: usize, ident: &Ident, ty: &Type) -> syn::Result<TokenStream2> {
    if is_value_slice(ty) || is_value_vec_ref(ty) {
        Ok(quote! {
            let Some(#ident) = params.get(#index).and_then(Value::as_array) else {
                return Value::Null;
            };
        })
    } else if is_value_ref(ty) {
        Ok(quote! {
            let Some(#ident) = params.get(#index) else {
                return Value::Null;
            };
        })
    } else if is_option_value_ref(ty) {
        Ok(quote! {
            let #ident = params.get(#index);
        })
    } else {
        Err(syn::Error::new_spanned(
            ty,
            format!("unsupported fn_info parameter type `{}`", type_text(ty)),
        ))
    }
}

fn return_type_expr(output: &ReturnType) -> syn::Result<TokenStream2> {
    match output {
        ReturnType::Default => Ok(quote! { JsonTypeInfo::Null }),
        ReturnType::Type(_, ty) => json_type_info_expr(ty).ok_or_else(|| {
            syn::Error::new_spanned(
                ty,
                format!("unsupported fn_info return type `{}`", type_text(ty)),
            )
        }),
    }
}

fn return_value_expr(
    fn_name: &Ident,
    call_args: &[TokenStream2],
    output: &ReturnType,
) -> syn::Result<TokenStream2> {
    match output {
        ReturnType::Default => Ok(quote! {
            #fn_name(#(#call_args),*);
            Value::Null
        }),
        ReturnType::Type(_, ty) if is_value(ty) => Ok(quote! {
            #fn_name(#(#call_args),*)
        }),
        ReturnType::Type(_, ty) if is_string(ty) => Ok(quote! {
            Value::String(#fn_name(#(#call_args),*))
        }),
        ReturnType::Type(_, ty) if is_option_usize(ty) => Ok(quote! {
            #fn_name(#(#call_args),*)
                .map(|value| serde_json::json!(value))
                .unwrap_or(Value::Null)
        }),
        ReturnType::Type(_, ty) => Err(syn::Error::new_spanned(
            ty,
            format!("unsupported fn_info return type `{}`", type_text(ty)),
        )),
    }
}

fn is_value(ty: &Type) -> bool {
    type_text(ty) == "Value"
}

fn is_string(ty: &Type) -> bool {
    type_text(ty) == "String"
}

fn is_value_ref(ty: &Type) -> bool {
    type_text(ty) == "& Value"
}

fn is_value_slice(ty: &Type) -> bool {
    type_text(ty) == "& [Value]"
}

fn is_value_vec_ref(ty: &Type) -> bool {
    matches!(type_text(ty).as_str(), "& Vec < Value >" | "& Vec<Value>")
}

fn is_option_value_ref(ty: &Type) -> bool {
    matches!(
        type_text(ty).as_str(),
        "Option < & Value >" | "Option<& Value>" | "Option<&Value>"
    )
}

fn is_option_usize(ty: &Type) -> bool {
    matches!(type_text(ty).as_str(), "Option < usize >" | "Option<usize>")
}

fn type_text(ty: &Type) -> String {
    ty.to_token_stream().to_string()
}
