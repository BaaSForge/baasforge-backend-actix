use crate::configs::AppEnvironment;
use crate::routes::AppContext;
use actix_web::{
    Error, HttpResponse,
    body::BoxBody,
    dev::{ServiceRequest, ServiceResponse},
    middleware::Next,
    web,
};

pub async fn test_closed_stage_guard_middleware<B>(
    req: ServiceRequest,
    next: Next<B>,
) -> Result<ServiceResponse<BoxBody>, Error>
where
    B: actix_web::body::MessageBody + 'static,
{
    let app_context = req
        .app_data::<web::Data<AppContext>>()
        .ok_or_else(|| actix_web::error::ErrorInternalServerError("Missing AppContext"))?;

    if app_context.settings.app_environment != AppEnvironment::Staging {
        return Ok(req.into_response(HttpResponse::Unauthorized().finish()));
    }

    let res = next.call(req).await?;
    Ok(res.map_into_boxed_body())
}
