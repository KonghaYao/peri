use url::Url;

use crate::{ModelError, ModelResult, ProtocolErrorKind};

#[path = "route/model_transport_route.rs"]
mod model_transport_route;
pub use model_transport_route::ModelTransportRoute;
