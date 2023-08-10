use actix_web::{
    get,
    http::header::ContentType,
    route,
    web::{self, Bytes},
    HttpResponse, Responder,
};
use serde::Deserialize;
use std::sync::Arc;
use trustification_api::search::SearchOptions;
use trustification_auth::{
    authenticator::{user::UserDetails, Authenticator},
    authorizer::Authorizer,
    swagger_ui::SwaggerUiOidc,
    ROLE_MANAGER,
};
use trustification_infrastructure::new_auth;
use trustification_storage::Storage;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;
use vexination_model::prelude::*;

use crate::SharedState;

#[derive(OpenApi)]
#[openapi(
    paths(fetch_vex, publish_vex, search_vex),
    components(schemas(SearchDocument, SearchResult),)
)]
pub struct ApiDoc;

pub fn config(
    cfg: &mut web::ServiceConfig,
    auth: Option<Arc<Authenticator>>,
    swagger_ui_oidc: Option<Arc<SwaggerUiOidc>>,
) {
    cfg.service(
        web::scope("/api/v1")
            .wrap(new_auth!(auth))
            .app_data(web::PayloadConfig::new(10 * 1024 * 1024))
            .service(fetch_vex)
            .service(publish_vex)
            .service(search_vex),
    )
    .service({
        let mut openapi = ApiDoc::openapi();
        let mut swagger = SwaggerUi::new("/swagger-ui/{_:.*}");

        if let Some(swagger_ui_oidc) = &swagger_ui_oidc {
            swagger = swagger_ui_oidc.apply(swagger, &mut openapi);
        }

        swagger.url("/openapi.json", openapi)
    });
}

async fn fetch_object(storage: &Storage, key: &str) -> HttpResponse {
    match storage.get_decoded_stream(key).await {
        Ok(stream) => HttpResponse::Ok().content_type(ContentType::json()).streaming(stream),
        Err(e) => {
            log::warn!("Unable to locate object with key {}: {:?}", key, e);
            HttpResponse::NotFound().finish()
        }
    }
}

/// Parameters passed when fetching an advisory.
#[derive(Debug, Deserialize)]
struct QueryParams {
    /// Identifier of the advisory to get
    advisory: String,
}

/// Retrieve an SBOM using its identifier.
#[utoipa::path(
    get,
    tag = "vexination",
    path = "/api/v1/vex",
    responses(
        (status = 200, description = "VEX found"),
        (status = NOT_FOUND, description = "VEX not found in archive"),
        (status = BAD_REQUEST, description = "Missing valid id or index entry"),
    ),
    params(
        ("advisory" = String, Query, description = "Identifier of VEX to fetch"),
    )
)]
#[get("/vex")]
async fn fetch_vex(state: web::Data<SharedState>, params: web::Query<QueryParams>) -> HttpResponse {
    let storage = state.storage.read().await;
    fetch_object(&storage, &params.advisory).await
}

/// Parameters passed when publishing advisory.
#[derive(Debug, Deserialize)]
struct PublishParams {
    /// Optional: Advisory identifier (overrides identifier derived from document)
    advisory: Option<String>,
}

/// Upload a VEX document.
///
/// The document must be in the CSAF v2.0 format.
#[utoipa::path(
    put,
    tag = "vexination",
    path = "/api/v1/vex",
    request_body(content = Value, description = "The VEX doc to be uploaded", content_type = "application/json"),
    responses(
        (status = 200, description = "VEX uploaded successfully"),
        (status = BAD_REQUEST, description = "Missing valid id"),
    ),
    params(
        ("advisory" = String, Query, description = "Identifier assigned to the VEX"),
    )
)]
#[route("/vex", method = "PUT", method = "POST")]
async fn publish_vex(
    state: web::Data<SharedState>,
    params: web::Query<PublishParams>,
    data: Bytes,
    authorizer: web::Data<Authorizer>,
    user: Option<UserDetails>,
) -> actix_web::Result<HttpResponse> {
    authorizer.require_role(user, ROLE_MANAGER)?;

    let params = params.into_inner();
    let advisory = if let Some(advisory) = params.advisory {
        advisory.to_string()
    } else {
        match serde_json::from_slice::<csaf::Csaf>(&data) {
            Ok(data) => data.document.tracking.id,
            Err(e) => {
                log::warn!("Unknown input format: {:?}", e);
                return Ok(HttpResponse::BadRequest().into());
            }
        }
    };

    let storage = state.storage.write().await;
    log::debug!("Storing new VEX with id: {advisory}");
    Ok(match storage.put_json_slice(&advisory, &data).await {
        Ok(_) => {
            let msg = format!("VEX of size {} stored successfully", &data[..].len());
            log::trace!("{}", msg);
            HttpResponse::Created().body(msg)
        }
        Err(e) => {
            let msg = format!("Error storing VEX: {:?}", e);
            log::warn!("{}", msg);
            HttpResponse::InternalServerError().body(msg)
        }
    })
}

/// Parameters for search query.
#[derive(Debug, Deserialize)]
pub struct SearchParams {
    /// Search query string
    pub q: String,
    /// Offset of documents to return (for pagination)
    #[serde(default = "default_offset")]
    pub offset: usize,
    /// Max number of documents to return
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Provide a detailed explanation of query matches
    #[serde(default = "default_explain")]
    pub explain: bool,
    /// Provide additional metadata from the index
    #[serde(default = "default_metadata")]
    pub metadata: bool,
}

const fn default_offset() -> usize {
    0
}

const fn default_limit() -> usize {
    10
}

const fn default_explain() -> bool {
    false
}

const fn default_metadata() -> bool {
    false
}

impl From<&SearchParams> for SearchOptions {
    fn from(value: &SearchParams) -> Self {
        Self {
            explain: value.explain,
            metadata: value.metadata,
        }
    }
}

/// Search for a VEX using a free form search query.
///
/// See the [documentation](https://docs.trustification.dev/trustification/user/retrieve.html) for a description of the query language.
#[utoipa::path(
    get,
    tag = "vexination",
    path = "/api/v1/vex/search",
    responses(
        (status = 200, description = "Search completed"),
        (status = BAD_REQUEST, description = "Bad query"),
    ),
    params(
        ("q" = String, Query, description = "Search query"),
    )
)]
#[get("/vex/search")]
async fn search_vex(state: web::Data<SharedState>, params: web::Query<SearchParams>) -> impl Responder {
    let params = params.into_inner();

    log::info!("Querying VEX using {}", params.q);

    let index = state.index.read().await;
    let result = index.search(&params.q, params.offset, params.limit, (&params).into());

    let (result, total) = match result {
        Err(e) => {
            log::info!("Error searching: {:?}", e);
            return HttpResponse::InternalServerError().body(e.to_string());
        }
        Ok(result) => result,
    };

    HttpResponse::Ok().json(SearchResult { total, result })
}
