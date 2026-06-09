use crate::libs::api_response::{api_error, api_success};
use crate::middlewares::auth::AccessPayload;
use crate::services::chat_worker::llm::tools::get_package_type_definition_file_contents::{
    ToolGetPackageTypeDefinitionFileContentsPayload,
    fill_package_type_definition_file_contents_from_files,
};
use crate::services::chat_worker::llm::tools::get_packages_type_definition_files::{
    ToolGetPackagesTypeDefinitionFilesPayload, fill_package_type_definition_files_from_packages,
};
use crate::tsgo::package_type_definition_files::validate_node_modules_file_paths;
use actix_web::{HttpMessage, HttpRequest, HttpResponse, web};
use serde::Deserialize;
use serde_json::json;

const MAX_PACKAGES_TYPE_DEFINITION_FILES_FILL: usize = 20;
const MAX_PACKAGE_TYPE_DEFINITION_FILE_CONTENTS_FILL: usize = 20;

#[derive(Debug, Deserialize)]
pub struct PackagesTypeDefinitionFilesFillBody {
    pub packages: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PackageTypeDefinitionFileContentsFillBody {
    pub files: Vec<String>,
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

/// Returns the same hydrated tool payload sent to the LLM for `get_package_type_definition_file_contents`.
pub async fn handle_package_type_definition_file_contents(
    req: HttpRequest,
    body: web::Json<PackageTypeDefinitionFileContentsFillBody>,
) -> HttpResponse {
    let _access = req
        .extensions()
        .get::<AccessPayload>()
        .expect("AccessPayload must exist to fill package type definition file contents");

    let files = &body.files;

    if files.is_empty() {
        return HttpResponse::BadRequest().json(api_error(
            "`files` must contain at least one file path",
            json!({}),
        ));
    }
    if files.len() > MAX_PACKAGE_TYPE_DEFINITION_FILE_CONTENTS_FILL {
        return HttpResponse::BadRequest().json(api_error(
            "Too many file paths in one request",
            json!({ "max": MAX_PACKAGE_TYPE_DEFINITION_FILE_CONTENTS_FILL }),
        ));
    }
    if let Err(message) = validate_node_modules_file_paths(files) {
        return HttpResponse::BadRequest().json(api_error(&message, json!({})));
    }

    let payload: ToolGetPackageTypeDefinitionFileContentsPayload =
        fill_package_type_definition_file_contents_from_files(files);

    HttpResponse::Ok().json(api_success(
        "Package type definition file contents filled successfully",
        json!(payload),
    ))
}
