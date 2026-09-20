use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    parse_macro_input, Expr, FnArg, Ident, ImplItemFn, ItemFn, LitBool, Pat, Path, Token,
};

/// 解析 `#[cache_evict]` 的参数
///
/// 支持的语法:
/// - `#[cache_evict(UserBo, page)]`
/// - `#[cache_evict(UserBo, page = true)]`
/// - `#[cache_evict(UserBo, id = param.payload.id, all = true)]`
/// - `#[cache_evict(target = UserBo, id = param.payload.id, all = true, ctx = &param.context, cache = self.cache)]`
struct CacheEvictArgs {
    target: Path,
    id: Option<Expr>,
    page: bool,
    all: bool,
    ctx: Option<Expr>,
    cache: Option<Expr>,
}

impl Parse for CacheEvictArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut target: Option<Path> = None;
        let mut id: Option<Expr> = None;
        let mut page = false;
        let mut all = false;
        let mut ctx: Option<Expr> = None;
        let mut cache: Option<Expr> = None;

        let mut is_first = true;

        while !input.is_empty() {
            if is_first && !input.peek2(Token![=]) {
                // 首个参数是位置参数: 类型路径，例如 UserBo
                target = Some(input.parse::<Path>()?);
                is_first = false;
                if input.peek(Token![,]) {
                    input.parse::<Token![,]>()?;
                }
                continue;
            }
            is_first = false;

            let ident = input.parse::<Ident>()?;
            let key = ident.to_string();

            if input.peek(Token![=]) {
                input.parse::<Token![=]>()?;
                match key.as_str() {
                    "target" => {
                        target = Some(input.parse::<Path>()?);
                    }
                    "id" => {
                        id = Some(input.parse::<Expr>()?);
                    }
                    "page" => {
                        let lit = input.parse::<LitBool>()?;
                        page = lit.value();
                    }
                    "all" => {
                        let lit = input.parse::<LitBool>()?;
                        all = lit.value();
                    }
                    "ctx" => {
                        ctx = Some(input.parse::<Expr>()?);
                    }
                    "cache" => {
                        cache = Some(input.parse::<Expr>()?);
                    }
                    _ => {
                        return Err(syn::Error::new(
                            ident.span(),
                            format!("未知参数 `{key}`，支持 target, id, page, all, ctx, cache"),
                        ));
                    }
                }
            } else {
                // 布尔开关简化语法，例如 `page` 或 `all`
                match key.as_str() {
                    "page" => page = true,
                    "all" => all = true,
                    _ => {
                        return Err(syn::Error::new(
                            ident.span(),
                            format!("参数 `{key}` 需赋值，如 `{key} = ...`"),
                        ));
                    }
                }
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        let target = target.ok_or_else(|| {
            syn::Error::new(input.span(), "必须指定 target 类型，例如 `UserBo` 或 `target = UserBo`")
        })?;

        Ok(Self {
            target,
            id,
            page,
            all,
            ctx,
            cache,
        })
    }
}

/// 解析 `#[cacheable]` 的参数
///
/// 支持的语法:
/// - `#[cacheable(UserBo, id = param.payload.id)]`
/// - `#[cacheable(target = UserBo, id = param.payload.id, ctx = &param.context, cache = self.cache)]`
struct CacheableArgs {
    target: Path,
    id: Expr,
    ctx: Option<Expr>,
    cache: Option<Expr>,
}

impl Parse for CacheableArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut target: Option<Path> = None;
        let mut id: Option<Expr> = None;
        let mut ctx: Option<Expr> = None;
        let mut cache: Option<Expr> = None;

        let mut is_first = true;

        while !input.is_empty() {
            if is_first && !input.peek2(Token![=]) {
                target = Some(input.parse::<Path>()?);
                is_first = false;
                if input.peek(Token![,]) {
                    input.parse::<Token![,]>()?;
                }
                continue;
            }
            is_first = false;

            let ident = input.parse::<Ident>()?;
            let key = ident.to_string();

            input.parse::<Token![=]>()?;
            match key.as_str() {
                "target" => {
                    target = Some(input.parse::<Path>()?);
                }
                "id" => {
                    id = Some(input.parse::<Expr>()?);
                }
                "ctx" => {
                    ctx = Some(input.parse::<Expr>()?);
                }
                "cache" => {
                    cache = Some(input.parse::<Expr>()?);
                }
                _ => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("未知参数 `{key}`，支持 target, id, ctx, cache"),
                    ));
                }
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        let target = target.ok_or_else(|| {
            syn::Error::new(input.span(), "必须指定 target 类型，例如 `UserBo`")
        })?;
        let id = id.ok_or_else(|| {
            syn::Error::new(input.span(), "必须指定 id 表达式，例如 `id = param.payload.id`")
        })?;

        Ok(Self {
            target,
            id,
            ctx,
            cache,
        })
    }
}

/// 根据入参推导上下文 ReqCtx 引用
fn deduce_ctx(inputs: &Punctuated<FnArg, Token![,]>, explicit_ctx: Option<Expr>) -> proc_macro2::TokenStream {
    if let Some(ctx) = explicit_ctx {
        return quote! { #ctx };
    }
    for arg in inputs {
        if let FnArg::Typed(pat_type) = arg {
            if let Pat::Ident(pat_ident) = &*pat_type.pat {
                let name = pat_ident.ident.to_string();
                if name == "param" {
                    return quote! { &param.context };
                }
                if name == "ctx" {
                    return quote! { &ctx };
                }
            }
        }
    }
    // 默认回退到 &param.context
    quote! { &param.context }
}

/// 根据推导缓存对象
fn deduce_cache(explicit_cache: Option<Expr>) -> proc_macro2::TokenStream {
    if let Some(cache) = explicit_cache {
        quote! { #cache }
    } else {
        quote! { self.cache }
    }
}

/// 声明式写操作缓存失效属性宏
///
/// 当方法返回 `Ok` 时触发失效；返回 `Err` 则不影响缓存。
#[proc_macro_attribute]
pub fn cache_evict(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as CacheEvictArgs);

    if let Ok(mut impl_fn) = syn::parse::<ImplItemFn>(item.clone()) {
        let target = &args.target;
        let ctx = deduce_ctx(&impl_fn.sig.inputs, args.ctx);
        let cache = deduce_cache(args.cache);

        let evict_stmt = if args.all {
            if let Some(ref id_expr) = args.id {
                quote! {
                    let __id_opt = crate::common::cache::ToCacheIdOpt::to_cache_id_opt(&(#id_expr));
                    let _ = #cache.evict_all_smart::<#target>(__id_opt, #ctx).await;
                }
            } else {
                quote! {
                    let _ = #cache.evict_page::<#target>(#ctx).await;
                }
            }
        } else if args.page {
            quote! {
                let _ = #cache.evict_page::<#target>(#ctx).await;
            }
        } else if let Some(ref id_expr) = args.id {
            quote! {
                let __id_opt = crate::common::cache::ToCacheIdOpt::to_cache_id_opt(&(#id_expr));
                if let Some(__id_str) = __id_opt {
                    let _ = #cache.evict_with_ctx::<#target>(__id_str, #ctx).await;
                }
            }
        } else {
            quote! {}
        };

        let orig_stmts = impl_fn.block.stmts;
        let new_block = syn::parse_quote!({
            let __res = async move {
                #(#orig_stmts)*
            }.await;

            if __res.is_ok() {
                #evict_stmt
            }

            __res
        });

        impl_fn.block = new_block;
        return TokenStream::from(quote! { #impl_fn });
    }

    if let Ok(mut item_fn) = syn::parse::<ItemFn>(item) {
        let target = &args.target;
        let ctx = deduce_ctx(&item_fn.sig.inputs, args.ctx);
        let cache = deduce_cache(args.cache);

        let evict_stmt = if args.all {
            if let Some(ref id_expr) = args.id {
                quote! {
                    let __id_opt = crate::common::cache::ToCacheIdOpt::to_cache_id_opt(&(#id_expr));
                    let _ = #cache.evict_all_smart::<#target>(__id_opt, #ctx).await;
                }
            } else {
                quote! {
                    let _ = #cache.evict_page::<#target>(#ctx).await;
                }
            }
        } else if args.page {
            quote! {
                let _ = #cache.evict_page::<#target>(#ctx).await;
            }
        } else if let Some(ref id_expr) = args.id {
            quote! {
                let __id_opt = crate::common::cache::ToCacheIdOpt::to_cache_id_opt(&(#id_expr));
                if let Some(__id_str) = __id_opt {
                    let _ = #cache.evict_with_ctx::<#target>(__id_str, #ctx).await;
                }
            }
        } else {
            quote! {}
        };

        let orig_stmts = item_fn.block.stmts;
        let new_block = syn::parse_quote!({
            let __res = async move {
                #(#orig_stmts)*
            }.await;

            if __res.is_ok() {
                #evict_stmt
            }

            __res
        });

        item_fn.block = Box::new(new_block);
        return TokenStream::from(quote! { #item_fn });
    }

    syn::Error::new(proc_macro2::Span::call_site(), "#[cache_evict] 仅支持修饰 async 函数或方法")
        .to_compile_error()
        .into()
}

/// 声明式读操作透明穿透与自动回填属性宏
///
/// 遵循 Cache-Aside 模式：先查缓存，命中则返回；未命中则查库并自动回填。
#[proc_macro_attribute]
pub fn cacheable(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as CacheableArgs);

    if let Ok(mut impl_fn) = syn::parse::<ImplItemFn>(item.clone()) {
        let target = &args.target;
        let id_expr = &args.id;
        let ctx = deduce_ctx(&impl_fn.sig.inputs, args.ctx);
        let cache = deduce_cache(args.cache);

        let orig_stmts = impl_fn.block.stmts;
        let fallback_stmts = orig_stmts.clone();

        let new_block = syn::parse_quote!({
            let __id_opt = crate::common::cache::ToCacheIdOpt::to_cache_id_opt(&(#id_expr));
            if let Some(__id) = __id_opt {
                let __cache_res = #cache.get_or_load_with_ctx::<#target, _, _>(__id, #ctx, || async move {
                    let __inner_res = async move {
                        #(#orig_stmts)*
                    }.await;
                    match __inner_res {
                        Ok(__val) => Ok(Some(__val)),
                        Err(crate::common::error::AppError::NotFound(_)) => Ok(None),
                        Err(e) => Err(e),
                    }
                }).await?;

                return __cache_res.ok_or_else(|| crate::common::error::AppError::not_found("记录不存在"));
            }

            async move {
                #(#fallback_stmts)*
            }.await
        });

        impl_fn.block = new_block;
        return TokenStream::from(quote! { #impl_fn });
    }

    if let Ok(mut item_fn) = syn::parse::<ItemFn>(item) {
        let target = &args.target;
        let id_expr = &args.id;
        let ctx = deduce_ctx(&item_fn.sig.inputs, args.ctx);
        let cache = deduce_cache(args.cache);

        let orig_stmts = item_fn.block.stmts;
        let fallback_stmts = orig_stmts.clone();

        let new_block = syn::parse_quote!({
            let __id_opt = crate::common::cache::ToCacheIdOpt::to_cache_id_opt(&(#id_expr));
            if let Some(__id) = __id_opt {
                let __cache_res = #cache.get_or_load_with_ctx::<#target, _, _>(__id, #ctx, || async move {
                    let __inner_res = async move {
                        #(#orig_stmts)*
                    }.await;
                    match __inner_res {
                        Ok(__val) => Ok(Some(__val)),
                        Err(crate::common::error::AppError::NotFound(_)) => Ok(None),
                        Err(e) => Err(e),
                    }
                }).await?;

                return __cache_res.ok_or_else(|| crate::common::error::AppError::not_found("记录不存在"));
            }

            async move {
                #(#fallback_stmts)*
            }.await
        });

        item_fn.block = Box::new(new_block);
        return TokenStream::from(quote! { #item_fn });
    }

    syn::Error::new(proc_macro2::Span::call_site(), "#[cacheable] 仅支持修饰 async 函数或方法")
        .to_compile_error()
        .into()
}
