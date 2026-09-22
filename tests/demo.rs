//! # pecs-macro 属性宏运行原理与实战 Demo
//!
//! 本文件是一个完全独立、可运行的端到端集成测试，直观演示：
//! 1. `#[cacheable]` 是如何截获查询、检查缓存、自动穿透并在命中时跳过数据库查库的；
//! 2. `#[cache_evict]` 是如何在更新/删除成功后自动触发缓存失效的；
//! 3. 宏展开前后的执行链路与 AOP 切面工作原理。

use pecs_macro::{cache_evict, cacheable};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

// =========================================================================
// 1. Mock 运行时环境（模拟项目中的 common::cache 与 common::error）
// =========================================================================

pub mod common {
    pub mod cache {
        /// 宏展开后统一调用此 Trait 将入参主键转换为 Option<String>
        pub trait ToCacheIdOpt {
            fn to_cache_id_opt(&self) -> Option<String>;
        }

        impl ToCacheIdOpt for u64 {
            fn to_cache_id_opt(&self) -> Option<String> {
                Some(self.to_string())
            }
        }

        impl ToCacheIdOpt for &str {
            fn to_cache_id_opt(&self) -> Option<String> {
                Some(self.to_string())
            }
        }
    }

    pub mod error {
        #[derive(Debug, PartialEq, Eq, Clone)]
        pub enum AppError {
            NotFound(String),
            DatabaseError(String),
        }

        impl AppError {
            pub fn not_found(msg: &str) -> Self {
                Self::NotFound(msg.to_string())
            }
        }
    }
}

use common::error::AppError;

/// 模拟请求上下文（包含了当前请求人凭据等）
#[derive(Clone, Default)]
pub struct ReqCtx;

/// 模拟传输载荷 ServiceRequest
pub struct ServiceRequest<T> {
    pub payload: T,
    pub context: ReqCtx,
}

/// 模拟底层缓存后端 (Mock CacheBackend)
#[derive(Clone, Default)]
pub struct MockCache {
    /// 模拟 KV 存储（如 Redis）
    store: Arc<Mutex<HashMap<String, String>>>,
    /// 记录实际查库（loader 执行）的次数
    pub db_call_count: Arc<Mutex<usize>>,
    /// 记录缓存被淘汰的次数
    pub evict_call_count: Arc<Mutex<usize>>,
}

impl MockCache {
    /// 对应真实 DynamicCache::get_or_load_with_ctx 方法
    pub async fn get_or_load_with_ctx<T, F, Fut>(
        &self,
        id: impl ToString,
        _ctx: &ReqCtx,
        loader: F,
    ) -> Result<Option<T>, AppError>
    where
        T: serde::de::DeserializeOwned + serde::Serialize,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<T>, AppError>>,
    {
        let key = id.to_string();

        // 1. 先查缓存（命中直接返回，不调 loader）
        {
            let map = self.store.lock().unwrap();
            if let Some(json) = map.get(&key) {
                let val: T = serde_json::from_str(json).unwrap();
                return Ok(Some(val));
            }
        }

        // 2. 缓存未命中：执行原函数包装后的 loader（查库）
        *self.db_call_count.lock().unwrap() += 1;
        let res = loader().await?;

        // 3. 回填缓存
        if let Some(ref val) = res {
            let json = serde_json::to_string(val).unwrap();
            self.store.lock().unwrap().insert(key, json);
        }

        Ok(res)
    }

    /// 对应真实 DynamicCache::evict_all_smart 方法
    pub async fn evict_all_smart<T>(
        &self,
        id: Option<String>,
        _ctx: &ReqCtx,
    ) -> Result<(), AppError> {
        *self.evict_call_count.lock().unwrap() += 1;
        if let Some(key) = id {
            self.store.lock().unwrap().remove(&key);
        }
        Ok(())
    }

    /// 对应真实 DynamicCache::evict_page 方法
    pub async fn evict_page<T>(&self, _ctx: &ReqCtx) -> Result<(), AppError> {
        *self.evict_call_count.lock().unwrap() += 1;
        Ok(())
    }
}

// =========================================================================
// 2. 业务领域对象与领域服务定义 (Domain & Biz)
// =========================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserBo {
    pub id: u64,
    pub name: String,
}

pub struct UserBiz {
    pub cache: MockCache,
}

impl UserBiz {
    /// 读操作演示：
    /// 挂载 `#[cacheable(UserBo, id = param.payload.id)]`
    ///
    /// 【宏展开前】开发者只需专注于编写获取数据的逻辑（如查库）：
    /// 【宏展开后】编译器自动在外部套一层：先去 cache 查，查到了直接 return，查不到才执行下面这行代码并写入 cache！
    #[cacheable(UserBo, id = param.payload.id)]
    pub async fn get(&self, param: &ServiceRequest<UserBo>) -> Result<UserBo, AppError> {
        // 模拟真实 SQL 查库
        Ok(UserBo {
            id: param.payload.id,
            name: format!("User_{}", param.payload.id),
        })
    }

    /// 写操作演示：
    /// 挂载 `#[cache_evict(UserBo, id = param.payload.id, all = true)]`
    ///
    /// 【宏展开前】开发者专注于编写更新逻辑；
    /// 【宏展开后】宏先执行原函数，若返回 Ok，则自动调用 `cache.evict_all_smart` 淘汰 Redis 缓存！
    #[cache_evict(UserBo, id = param.payload.id, all = true)]
    pub async fn update(&self, param: &ServiceRequest<UserBo>) -> Result<UserBo, AppError> {
        // 模拟真实 SQL 更新
        Ok(param.payload.clone())
    }

    /// 失败操作演示：更新失败时绝不会误删缓存
    #[cache_evict(UserBo, id = param.payload.id, all = true)]
    pub async fn update_fail(&self, param: &ServiceRequest<UserBo>) -> Result<UserBo, AppError> {
        // 模拟数据库报错
        Err(AppError::DatabaseError("数据库死锁".into()))
    }
}

// =========================================================================
// 3. 单元测试验证全流程行为
// =========================================================================

#[tokio::test]
async fn test_cacheable_and_evict_full_lifecycle() {
    let cache = MockCache::default();
    let biz = UserBiz {
        cache: cache.clone(),
    };

    let req = ServiceRequest {
        payload: UserBo {
            id: 1001,
            name: "Alice".into(),
        },
        context: ReqCtx,
    };

    // -------------------------------------------------------------
    // 第 1 步：第一次发起 get 查询 -> 缓存未命中，执行查库，写入缓存
    // -------------------------------------------------------------
    let user = biz.get(&req).await.unwrap();
    assert_eq!(user.name, "User_1001");
    // 验证：真实查库计数器增加为 1
    assert_eq!(*cache.db_call_count.lock().unwrap(), 1);

    // -------------------------------------------------------------
    // 第 2 步：第二次发起相同 get 查询 -> 命中缓存！
    // -------------------------------------------------------------
    let user_cached = biz.get(&req).await.unwrap();
    assert_eq!(user_cached.name, "User_1001");
    // 验证：查库计数器依然为 1，原函数体内的代码根本没有被重复执行！
    assert_eq!(*cache.db_call_count.lock().unwrap(), 1);

    // -------------------------------------------------------------
    // 第 3 步：更新失败（返回 Err） -> 宏不触发失效，保护已有缓存
    // -------------------------------------------------------------
    let fail_res = biz.update_fail(&req).await;
    assert!(fail_res.is_err());
    // 验证：淘汰计数器依然为 0
    assert_eq!(*cache.evict_call_count.lock().unwrap(), 0);

    // -------------------------------------------------------------
    // 第 4 步：更新成功（返回 Ok） -> 宏自动触发缓存失效（淘汰）
    // -------------------------------------------------------------
    let update_req = ServiceRequest {
        payload: UserBo {
            id: 1001,
            name: "Bob".into(),
        },
        context: ReqCtx,
    };
    let updated = biz.update(&update_req).await.unwrap();
    assert_eq!(updated.name, "Bob");
    // 验证：淘汰计数器增加为 1，对应 Key 已从 MockCache 中被删除
    assert_eq!(*cache.evict_call_count.lock().unwrap(), 1);

    // -------------------------------------------------------------
    // 第 5 步：第三次发起 get 查询 -> 因上一步已被淘汰，再次触发查库回填
    // -------------------------------------------------------------
    let user_after_evict = biz.get(&req).await.unwrap();
    assert_eq!(user_after_evict.name, "User_1001");
    // 验证：查库计数器增加为 2
    assert_eq!(*cache.db_call_count.lock().unwrap(), 2);
}
