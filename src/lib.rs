//! # pecs-macro: 声明式缓存属性过程宏 (Declarative Caching Procedural Macros)
//!
//! ## 一、 宏的设计定位与运作原理 (Why & How it Works)
//!
//! 在 Rust 中，属性过程宏 (`#[proc_macro_attribute]`) 运行在**编译阶段**。
//! 编译器将修饰的代码函数解析为抽象语法树（AST），以 `TokenStream` 形式传递给本宏函数。
//! 本宏通过 `syn` 将其解析为结构化语法树，对其函数体进行**透明 AOP（面向切面）重写**，再通过 `quote!`
//! 重新生成替换后的 Rust 代码并交还给编译器。
//!
//! ### 1. 读操作 `#[cacheable]` 的切面包装机制：
//! - 宏将原函数内的全部语句截获，打包为一个闭包（即数据加载器 `loader`）；
//! - 生成先查缓存的逻辑：调用 `cache.get_or_load_with_ctx(id, ctx, loader)`；
//! - **缓存命中时**：直接返回已反序列化的缓存对象，**原函数体内的查库 SQL 根本不会被执行**！
//! - **缓存未命中时**：自动执行原函数体，查库成功后自动回填缓存，并返回结果。
//!
//! ### 2. 写操作 `#[cache_evict]` 的切面拦截机制：
//! - 宏先执行原函数的更新/删除逻辑：`let __res = async move { ... 原函数体 ... }.await;`
//! - **仅当执行结果为 `Ok` 时**：触发缓存失效动作（调用 `cache.evict_...` 淘汰 Redis 对应 Key）；
//! - **若数据库执行失败（返回 `Err`）**：自动跳过失效，保持原有缓存，避免不一致。
//!
//! ---

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    parse_macro_input, Expr, FnArg, Ident, ImplItemFn, ItemFn, LitBool, Pat, Path, Token,
};

/// 解析 `#[cache_evict]` 属性宏入参的数据结构
///
/// 支持丰富的书写语法：
/// 1. 位置参数与布尔简化：
///    - `#[cache_evict(UserBo, page)]` -> 仅失效分页列表缓存
///    - `#[cache_evict(UserBo, id = param.payload.id, all = true)]` -> 同时失效详情与分页
/// 2. 完整键值对形式：
///    - `#[cache_evict(target = UserBo, id = param.payload.id, all, ctx = &param.context, cache = self.cache)]`
struct CacheEvictArgs {
    /// 目标实体类型（如 `UserBo`），用于提取其 `CachePolicy` 标记推导业务名与策略
    target: Path,
    /// 详情缓存的主键 ID 表达式（如 `param.payload.id`）
    id: Option<Expr>,
    /// 是否仅失效对应业务的分页前缀缓存
    page: bool,
    /// 是否同时失效详情缓存与关联的分页列表缓存（全量失效）
    all: bool,
    /// 显式指定请求上下文（可选，未指定时由 `deduce_ctx` 自动从方法参数推导）
    ctx: Option<Expr>,
    /// 显式指定缓存管理器实例（可选，未指定时默认为 `self.cache`）
    cache: Option<Expr>,
}

impl Parse for CacheEvictArgs {
    /// 自定义参数语法解析流程（消费 Token 流）
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut target: Option<Path> = None;
        let mut id: Option<Expr> = None;
        let mut page = false;
        let mut all = false;
        let mut ctx: Option<Expr> = None;
        let mut cache: Option<Expr> = None;

        let mut is_first = true;

        while !input.is_empty() {
            // 支持第 1 个参数直接写类型名称（如 `#[cache_evict(UserBo, ...)]`）
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

            if input.peek(Token![=]) {
                // 处理形如 `key = value` 的参数
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
                // 处理无赋值的纯布尔开关标记，例如 `page` 或 `all`
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

/// 解析 `#[cacheable]` 属性宏入参的数据结构
///
/// 支持的语法:
/// - `#[cacheable(UserBo, id = param.payload.id)]`
/// - `#[cacheable(target = UserBo, id = param.payload.id, ctx = &param.context, cache = self.cache)]`
struct CacheableArgs {
    /// 目标实体类型（如 `UserBo`）
    target: Path,
    /// 详情缓存的主键 ID 表达式（如 `param.payload.id`）
    id: Expr,
    /// 显式指定请求上下文（可选）
    ctx: Option<Expr>,
    /// 显式指定缓存实例（可选）
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

/// 智能推导当前方法调用的上下文 `ReqCtx` 引用
///
/// 优先级原则：
/// 1. 若宏参数显式传入了 `ctx = ...`，优先使用显式表达式；
/// 2. 遍历方法入参签名：
///    - 若入参名为 `param`（标准 ServiceRequest）：自动解析为 `&param.context`；
///    - 若入参名为 `ctx`（上下文入参）：自动解析为 `&ctx`；
/// 3. 兜底回退为 `&param.context`。
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
    quote! { &param.context }
}

/// 智能推导缓存管理器引用（默认约定为当前结构体中的 `self.cache`）
fn deduce_cache(explicit_cache: Option<Expr>) -> proc_macro2::TokenStream {
    if let Some(cache) = explicit_cache {
        quote! { #cache }
    } else {
        quote! { self.cache }
    }
}

/// 声明式写操作缓存失效属性宏
///
/// # 展开逻辑与工作流程 (Workflow):
/// ```text
/// 原始代码:
/// #[cache_evict(UserBo, id = param.payload.id, all)]
/// pub async fn update(&self, param: ...) -> AppResult<UserBo> {
///     /* 业务 SQL 更新逻辑 */
/// }
///
/// 展开后的实际编译代码:
/// pub async fn update(&self, param: ...) -> AppResult<UserBo> {
///     // 1. 先执行原函数内的业务逻辑
///     let __res = async move {
///         /* 业务 SQL 更新逻辑 */
///     }.await;
///
///     // 2. 只有业务更新成功返回 Ok 时，才触发主动缓存淘汰
///     if __res.is_ok() {
///         let __id_opt = ToCacheIdOpt::to_cache_id_opt(&(param.payload.id));
///         let _ = self.cache.evict_all_smart::<UserBo>(__id_opt, &param.context).await;
///     }
///
///     __res
/// }
/// ```
#[proc_macro_attribute]
pub fn cache_evict(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as CacheEvictArgs);

    // 场景 A: 修饰 `impl ...` 块中的成员方法
    if let Ok(mut impl_fn) = syn::parse::<ImplItemFn>(item.clone()) {
        let target = &args.target;
        let ctx = deduce_ctx(&impl_fn.sig.inputs, args.ctx);
        let cache = deduce_cache(args.cache);

        // 根据参数组合生成对应的失效调用语句
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

        // 提取原函数体内全部语句，包装在闭包内安全执行
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

    // 场景 B: 修饰独立顶级函数 `item_fn`
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
/// # 遵循 Cache-Aside 模式与展开后的工作流程:
/// ```text
/// 原始代码:
/// #[cacheable(UserBo, id = param.payload.id)]
/// pub async fn get(&self, param: &ServiceRequest<UserBo>) -> AppResult<UserBo> {
///     let entity = self.user_repo.get(...).await?;
///     Ok(entity.into())
/// }
///
/// 展开后的实际编译代码:
/// pub async fn get(&self, param: &ServiceRequest<UserBo>) -> AppResult<UserBo> {
///     let __id_opt = ToCacheIdOpt::to_cache_id_opt(&(param.payload.id));
///     if let Some(__id) = __id_opt {
///         // 拦截查缓存：命中直接返回，未命中执行 loader 闭包穿透查库并自动写回 Redis
///         let __cache_res = self.cache.get_or_load_with_ctx::<UserBo, _, _>(__id, &param.context, || async move {
///             // 原函数体被作为 loader 传入
///             let __inner_res = async move {
///                 let entity = self.user_repo.get(...).await?;
///                 Ok(entity.into())
///             }.await;
///
///             match __inner_res {
///                 Ok(__val) => Ok(Some(__val)),
///                 Err(AppError::NotFound(_)) => Ok(None),
///                 Err(e) => Err(e),
///             }
///         }).await?;
///
///         return __cache_res.ok_or_else(|| AppError::not_found("记录不存在"));
///     }
///
///     // 若主键为 None 则降级直接执行原逻辑
///     async move { ... }.await
/// }
/// ```
#[proc_macro_attribute]
pub fn cacheable(args: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as CacheableArgs);

    // 场景 A: 修饰 `impl ...` 块中的成员方法
    if let Ok(mut impl_fn) = syn::parse::<ImplItemFn>(item.clone()) {
        let target = &args.target;
        let id_expr = &args.id;
        let ctx = deduce_ctx(&impl_fn.sig.inputs, args.ctx);
        let cache = deduce_cache(args.cache);

        let orig_stmts = impl_fn.block.stmts;
        let fallback_stmts = orig_stmts.clone();

        let new_block = syn::parse_quote!({
            // 1. 将主键统一转换为 Option<String>
            let __id_opt = crate::common::cache::ToCacheIdOpt::to_cache_id_opt(&(#id_expr));
            if let Some(__id) = __id_opt {
                // 2. 先查缓存；未命中时执行原函数体 loader 并自动回填
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

                // 3. 缓存空值安全还原为 NotFound 错误
                return __cache_res.ok_or_else(|| crate::common::error::AppError::not_found("记录不存在"));
            }

            // 4. 若主键为 None 无法定位缓存，兜底直接执行原函数体
            async move {
                #(#fallback_stmts)*
            }.await
        });

        impl_fn.block = new_block;
        return TokenStream::from(quote! { #impl_fn });
    }

    // 场景 B: 修饰独立顶级函数 `item_fn`
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
