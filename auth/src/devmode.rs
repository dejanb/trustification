const ISSUER_URL: &str = "http://localhost:8090/realms/chicken";
pub const CLIENT_IDS: &[&str] = &["frontend", "walker"];
pub const SWAGGER_UI_CLIENT_ID: &str = "frontend";

pub const SCOPE_MAPPINGS: &[(&str, &[&str])] = &[
    ("create:document", &["create.sbom", "create.vex"]),
    ("read:document", &["read.sbom", "read.vex"]),
    ("update:document", &["update.sbom", "update.vex"]),
    ("delete:document", &["delete.sbom", "delete.vex"]),
];

pub fn issuer_url() -> String {
    std::env::var("ISSUER_URL").unwrap_or_else(|_| ISSUER_URL.to_string())
}