pub mod filesystem;
pub mod image;
pub mod terminal;
pub mod todo;
pub mod web;
pub(crate) mod web_common;
pub(crate) mod web_fetch;
pub(crate) mod web_search;

pub use filesystem::FilesystemMiddleware;
pub use image::ImageMiddleware;
pub use terminal::TerminalMiddleware;
pub use todo::TodoMiddleware;
pub use web::WebMiddleware;
