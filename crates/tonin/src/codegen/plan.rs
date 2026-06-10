// Re-exported from tonin-plugin so the rest of the CLI codegen module
// can continue to use `super::plan::Plan` / `super::plan::Mesh` etc.
// without import changes.
pub use tonin_plugin::{
    CURRENT_SCHEMA, ClientSpec, Error, Mesh, MethodCacheSpec, Plan, RECOMMENDED_CLI_MIN,
    SUPPORTED_SCHEMAS, ServiceKind, ServiceRef, WebMode,
};
