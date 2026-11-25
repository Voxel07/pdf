use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    Date,
    Number,
    Select,
    Signature,
}

/// A field placed on the template PDF. Coordinates are relative (0..1) to the displayed
/// page with the origin in the top-left corner (the same convention DocuSeal uses).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    /// Section (group) this field belongs to. Sections are filled & locked atomically.
    #[serde(default)]
    pub section: u32,
    /// 0-based page index.
    pub page: u32,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Options for `select` fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<f32>,
    /// Date output format, e.g. `DD.MM.YYYY` (default) or `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Filled automatically by the server: `person_name`, `today`, `discord_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autofill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Reference to the origin of an imported field (e.g. DocuSeal field uuid).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub id: u32,
    pub name: String,
    /// Required sections must be completed for the document to count as signed.
    /// Optional sections can be filled later ("continue the form").
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, FromRow)]
pub struct TemplateRow {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub validity_days: Option<i64>,
    pub page_count: i64,
    pub pdf_sha256: String,
    pub fields: String,
    pub sections: String,
    pub source: Option<String>,
    pub archived: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Template {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub validity_days: Option<i64>,
    pub page_count: i64,
    pub pdf_sha256: String,
    pub fields: Vec<Field>,
    pub sections: Vec<Section>,
    pub source: Option<String>,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl From<TemplateRow> for Template {
    fn from(r: TemplateRow) -> Self {
        let mut sections: Vec<Section> = serde_json::from_str(&r.sections).unwrap_or_default();
        if sections.is_empty() {
            sections.push(Section { id: 0, name: "Main".into(), required: true });
        }
        Template {
            id: r.id,
            slug: r.slug,
            name: r.name,
            description: r.description,
            validity_days: r.validity_days,
            page_count: r.page_count,
            pdf_sha256: r.pdf_sha256,
            fields: serde_json::from_str(&r.fields).unwrap_or_default(),
            sections,
            source: r.source,
            archived: r.archived != 0,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}


#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    pub zip: Option<String>,
    pub notes: String,
    pub created_at: String,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DiscordAccount {
    pub discord_id: String,
    pub person_id: String,
    pub username: Option<String>,
    pub linked_at: String,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Submission {
    pub id: String,
    pub template_id: String,
    pub person_id: String,
    pub token: String,
    pub status: String,
    pub failed_attempts: i64,
    pub source: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct FieldValue {
    pub field_id: String,
    pub value: String,
    pub image: Option<Vec<u8>>,
    pub round_id: i64,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Round {
    pub id: i64,
    pub submission_id: String,
    pub sections: String,
    pub signer_name: String,
    pub ip: String,
    pub user_agent: String,
    pub created_at: String,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct PublicLink {
    pub id: String,
    pub template_id: String,
    pub token: String,
    pub label: String,
    pub active: bool,
    pub expires_at: Option<String>,
    pub uses: i64,
    pub created_by: Option<String>,
    pub created_at: String,
}
