use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote, ToTokens};
use syn::{
    parse_macro_input, punctuated::Punctuated, FnArg, GenericArgument, Ident, ItemFn, Lit, Meta,
    Pat, PathArguments, ReturnType, Token, Type,
};

#[proc_macro_attribute]
pub fn fn_info(args: TokenStream, input: TokenStream) -> TokenStream {
    let function = parse_macro_input!(input as ItemFn);
    let args = parse_macro_input!(args with Punctuated::<Meta, Token![,]>::parse_terminated);
    expand_fn_info(args, function)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_fn_info(
    args: Punctuated<Meta, Token![,]>,
    function: ItemFn,
) -> syn::Result<TokenStream2> {
    let fn_name = &function.sig.ident;
    let info_name = format_ident!("{fn_name}_info");
    let function_name =
        function_info_name(&args)?.unwrap_or_else(|| default_function_name(fn_name));

    let mut params = Vec::new();
    let mut extract_args = Vec::new();
    let mut call_args = Vec::new();
    let mut min_params = 0usize;
    let function_inputs: Vec<_> = function
        .sig
        .inputs
        .iter()
        .map(|arg| match arg {
            FnArg::Typed(arg) => Ok(arg),
            FnArg::Receiver(_) => Err(syn::Error::new_spanned(
                arg,
                "fn_info does not support methods",
            )),
        })
        .collect::<syn::Result<_>>()?;
    let query_inputs: Vec<_> = function_inputs
        .iter()
        .copied()
        .filter(|arg| !is_meta_parameters(&arg.ty))
        .collect();
    let input_count = query_inputs.len();
    let variadic = input_count == 1
        && function
            .sig
            .inputs
            .iter()
            .all(|arg| matches!(arg, FnArg::Typed(arg) if is_meta_parameters(&arg.ty) || is_value_slice(&arg.ty) || is_value_vec_ref(&arg.ty)));

    let mut meta_parameters_seen = false;
    let mut query_index = 0usize;
    for arg in function_inputs {
        let Pat::Ident(pat) = arg.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &arg.pat,
                "fn_info only supports identifier parameters",
            ));
        };

        let ident = &pat.ident;
        if is_meta_parameters(&arg.ty) {
            if meta_parameters_seen {
                return Err(syn::Error::new_spanned(
                    &arg.ty,
                    "fn_info only supports one MetaParameters parameter",
                ));
            }
            meta_parameters_seen = true;
            call_args.push(quote! { metaparams });
            continue;
        }

        let name = ident.to_string();
        let type_info = type_info_expr(&arg.ty)?;
        let extraction = extraction_expr(query_index, input_count, ident, &arg.ty)?;
        let call_arg = call_arg_expr(ident, &arg.ty);

        if !is_option_value_ref(&arg.ty) && !is_option_usize(&arg.ty) {
            min_params = query_index + 1;
        }
        query_index += 1;

        params.push(quote! {
            ParamInfo {
                name: #name,
                r#type: #type_info,
            }
        });
        extract_args.push(extraction);
        call_args.push(call_arg);
    }

    let return_type = return_type_expr(&function.sig.output)?;
    let return_value = return_value_expr(fn_name, &call_args, &function.sig.output)?;

    Ok(quote! {
        #[allow(dead_code)]
        fn #info_name() -> FnInfo {
            FnInfo {
                name: #function_name,
                params: vec![#(#params),*],
                min_params: #min_params,
                variadic: #variadic,
                return_type: #return_type,
                function: Box::new(|params: &[Value], metaparams| {
                    #(#extract_args)*
                    #return_value
                }),
            }
        }

        #function
    })
}

fn function_info_name(args: &Punctuated<Meta, Token![,]>) -> syn::Result<Option<String>> {
    let mut name = None;
    for arg in args {
        let Meta::NameValue(name_value) = arg else {
            return Err(syn::Error::new_spanned(
                arg,
                "fn_info only supports `name = \"...\"` arguments",
            ));
        };

        if !name_value.path.is_ident("name") {
            return Err(syn::Error::new_spanned(
                &name_value.path,
                "unsupported fn_info argument",
            ));
        }

        let syn::Expr::Lit(expr) = &name_value.value else {
            return Err(syn::Error::new_spanned(
                &name_value.value,
                "fn_info name must be a string literal",
            ));
        };
        let Lit::Str(value) = &expr.lit else {
            return Err(syn::Error::new_spanned(
                &expr.lit,
                "fn_info name must be a string literal",
            ));
        };

        name = Some(value.value());
    }
    Ok(name)
}

fn default_function_name(ident: &Ident) -> String {
    ident.to_string().trim_start_matches("r#").to_string()
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
    } else if is_string(ty) || is_str_ref(ty) {
        Some(quote! { JsonTypeInfo::String })
    } else if is_value_slice(ty) || is_value_vec_ref(ty) {
        Some(quote! { JsonTypeInfo::Array(JsonTypeInfo::Any.into()) })
    } else if is_value_vec(ty) {
        Some(quote! { JsonTypeInfo::Array(JsonTypeInfo::Any.into()) })
    } else if is_f64_vec(ty) || is_f64_vec_ref(ty) {
        Some(quote! { JsonTypeInfo::Array(JsonTypeInfo::Number.into()) })
    } else if is_usize(ty) || is_i64(ty) || is_f64(ty) {
        Some(quote! { JsonTypeInfo::Number })
    } else if is_bool(ty) {
        Some(quote! { JsonTypeInfo::Bool })
    } else if is_option_usize(ty) || is_option_f64(ty) {
        Some(quote! { JsonTypeInfo::OneOf(vec![JsonTypeInfo::Number, JsonTypeInfo::Null]) })
    } else if let Some((left, right)) = one_of_types(ty) {
        let left = json_type_info_expr(left).unwrap_or_else(|| quote! { JsonTypeInfo::Any });
        let right = json_type_info_expr(right).unwrap_or_else(|| quote! { JsonTypeInfo::Any });
        Some(quote! { JsonTypeInfo::OneOf(vec![#left, #right]) })
    } else if is_deserializable_type(ty) {
        Some(quote! { JsonTypeInfo::Any })
    } else {
        None
    }
}

fn extraction_expr(
    index: usize,
    input_count: usize,
    ident: &Ident,
    ty: &Type,
) -> syn::Result<TokenStream2> {
    if is_value_slice(ty) || is_value_vec_ref(ty) {
        if input_count == 1 {
            Ok(quote! {
                let #ident = params;
            })
        } else {
            Ok(quote! {
                let Some(#ident) = params.get(#index).and_then(Value::as_array) else {
                    return Value::Null;
                };
            })
        }
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
    } else if is_str_ref(ty) {
        Ok(quote! {
            let Some(#ident) = params.get(#index).and_then(Value::as_str) else {
                return Value::Null;
            };
        })
    } else if is_string(ty) {
        Ok(quote! {
            let Some(#ident) = params.get(#index).and_then(Value::as_str).map(ToString::to_string) else {
                return Value::Null;
            };
        })
    } else if is_f64_vec(ty) || is_f64_vec_ref(ty) {
        Ok(quote! {
            let Some(#ident) = params
                .get(#index)
                .and_then(Value::as_array)
                .and_then(|values| values.iter().map(Value::as_f64).collect::<Option<Vec<_>>>())
            else {
                return Value::Null;
            };
        })
    } else if is_usize(ty) {
        Ok(quote! {
            let Some(#ident) = params
                .get(#index)
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
            else {
                return Value::Null;
            };
        })
    } else if is_i64(ty) {
        Ok(quote! {
            let Some(#ident) = params.get(#index).and_then(Value::as_i64) else {
                return Value::Null;
            };
        })
    } else if is_f64(ty) {
        Ok(quote! {
            let Some(#ident) = params.get(#index).and_then(Value::as_f64) else {
                return Value::Null;
            };
        })
    } else if is_bool(ty) {
        Ok(quote! {
            let Some(#ident) = params.get(#index).and_then(Value::as_bool) else {
                return Value::Null;
            };
        })
    } else if let Some((left, right)) = one_of_types(ty) {
        Ok(quote! {
            let Some(value) = params.get(#index) else {
                return Value::Null;
            };
            let #ident = if let Ok(value) = serde_json::from_value::<#left>(value.clone()) {
                OneOf::A(value)
            } else if let Ok(value) = serde_json::from_value::<#right>(value.clone()) {
                OneOf::B(value)
            } else {
                return Value::Null;
            };
        })
    } else if is_deserializable_type(ty) {
        Ok(quote! {
            let Some(#ident) = params
                .get(#index)
                .and_then(|value| serde_json::from_value::<#ty>(value.clone()).ok())
            else {
                return Value::Null;
            };
        })
    } else {
        Err(syn::Error::new_spanned(
            ty,
            format!("unsupported fn_info parameter type `{}`", type_text(ty)),
        ))
    }
}

fn call_arg_expr(ident: &Ident, ty: &Type) -> TokenStream2 {
    if is_f64_vec_ref(ty) {
        quote! { &#ident }
    } else {
        quote! { #ident }
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
        ReturnType::Type(_, ty) if is_value_vec(ty) => Ok(quote! {
            Value::Array(#fn_name(#(#call_args),*))
        }),
        ReturnType::Type(_, ty) if is_f64_vec(ty) => Ok(quote! {
            serde_json::json!(#fn_name(#(#call_args),*))
        }),
        ReturnType::Type(_, ty) if is_usize(ty) || is_i64(ty) || is_f64(ty) => Ok(quote! {
            serde_json::json!(#fn_name(#(#call_args),*))
        }),
        ReturnType::Type(_, ty) if is_bool(ty) => Ok(quote! {
            Value::Bool(#fn_name(#(#call_args),*))
        }),
        ReturnType::Type(_, ty) if is_option_usize(ty) || is_option_f64(ty) => Ok(quote! {
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
    normalized_type_text(ty) == "Value"
}

fn is_string(ty: &Type) -> bool {
    normalized_type_text(ty) == "String"
}

fn is_str_ref(ty: &Type) -> bool {
    normalized_type_text(ty) == "&str"
}

fn is_value_ref(ty: &Type) -> bool {
    normalized_type_text(ty) == "&Value"
}

fn is_value_slice(ty: &Type) -> bool {
    normalized_type_text(ty) == "&[Value]"
}

fn is_value_vec_ref(ty: &Type) -> bool {
    normalized_type_text(ty) == "&Vec<Value>"
}

fn is_value_vec(ty: &Type) -> bool {
    normalized_type_text(ty) == "Vec<Value>"
}

fn is_f64_vec_ref(ty: &Type) -> bool {
    normalized_type_text(ty) == "&Vec<f64>"
}

fn is_f64_vec(ty: &Type) -> bool {
    normalized_type_text(ty) == "Vec<f64>"
}

fn is_option_value_ref(ty: &Type) -> bool {
    normalized_type_text(ty) == "Option<&Value>"
}

fn is_option_usize(ty: &Type) -> bool {
    normalized_type_text(ty) == "Option<usize>"
}

fn is_option_f64(ty: &Type) -> bool {
    normalized_type_text(ty) == "Option<f64>"
}

fn is_usize(ty: &Type) -> bool {
    normalized_type_text(ty) == "usize"
}

fn is_i64(ty: &Type) -> bool {
    normalized_type_text(ty) == "i64"
}

fn is_f64(ty: &Type) -> bool {
    normalized_type_text(ty) == "f64"
}

fn is_bool(ty: &Type) -> bool {
    normalized_type_text(ty) == "bool"
}

fn is_meta_parameters(ty: &Type) -> bool {
    normalized_type_text(ty) == "MetaParameters"
}

fn one_of_types(ty: &Type) -> Option<(&Type, &Type)> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "OneOf" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    if args.args.len() != 2 {
        return None;
    }
    let mut args = args.args.iter();
    let GenericArgument::Type(left) = args.next()? else {
        return None;
    };
    let GenericArgument::Type(right) = args.next()? else {
        return None;
    };
    Some((left, right))
}

fn is_deserializable_type(ty: &Type) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    type_path.qself.is_none()
        && type_path.path.leading_colon.is_none()
        && type_path.path.segments.len() == 1
}

fn normalized_type_text(ty: &Type) -> String {
    type_text(ty).replace(' ', "")
}

fn type_text(ty: &Type) -> String {
    ty.to_token_stream().to_string()
}
