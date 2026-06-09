use crate::libs::api_response::{api_error, api_success};
use crate::middlewares::auth::AccessPayload;
use crate::services::chat_worker::llm::tools::get_packages_type_definition_files::{
    ToolGetPackagesTypeDefinitionFilesPayload, fill_package_type_definition_files_from_packages,
};
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use serde::Deserialize;
use serde_json::json;

const MAX_PACKAGES_TYPE_DEFINITION_FILES_FILL: usize = 20;

#[derive(Debug, Deserialize)]
pub struct PackagesTypeDefinitionFilesFillBody {
    pub packages: Vec<String>,
}

/// Returns the same hydrated tool payload sent to the LLM for `get_packages_type_definition_files`.
/// Platform-wide static manifest — not scoped to a project or chat session.
pub async fn handle_packages_type_definition_files(
    req: HttpRequest,
    body: web::Json<PackagesTypeDefinitionFilesFillBody>,
) -> HttpResponse {
    let _access = req
        .extensions()
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to fill packages type definition files");

    let packages = &body.packages;

    if packages.is_empty() {
        return HttpResponse::BadRequest().json(api_error(
            "`packages` must contain at least one package name",
            json!({}),
        ));
    }
    if packages.len() > MAX_PACKAGES_TYPE_DEFINITION_FILES_FILL {
        return HttpResponse::BadRequest().json(api_error(
            "Too many package names in one request",
            json!({ "max": MAX_PACKAGES_TYPE_DEFINITION_FILES_FILL }),
        ));
    }

    let payload: ToolGetPackagesTypeDefinitionFilesPayload =
        fill_package_type_definition_files_from_packages(packages);

    HttpResponse::Ok().json(api_success(
        "Packages type definition files filled successfully",
        json!(payload),
    ))
}
