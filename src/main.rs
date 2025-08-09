// src/main.rs
use actix_cors::Cors;
use actix_web::{web, App, HttpResponse, HttpServer, Result, middleware};
use actix_session::{Session, SessionMiddleware, storage::CookieSessionStore};
use actix_web::cookie::SameSite;
use anyhow::Context;
use chrono::{Utc, NaiveDate};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{postgres::PgPoolOptions, Pool, Postgres, Row, Column, ValueRef};
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use std::process::{Child, Command};
use std::time::{SystemTime, UNIX_EPOCH};
use std::path::Path;
use uuid::Uuid;
use url::Url;
use notify::{Watcher, RecursiveMode, RecommendedWatcher, Config as NotifyConfig};
use std::sync::mpsc::channel;
use oauth2::{AuthUrl, ClientId, ClientSecret, RedirectUrl, TokenUrl, CsrfToken, Scope};
use oauth2::basic::BasicClient;
use oauth2::reqwest::async_http_client;
use oauth2::{AuthorizationCode, TokenResponse};
use actix_web::cookie::time::Duration as CookieDuration;
use jsonwebtoken::{decode, Validation, DecodingKey, Algorithm};
use actix_web::{dev::ServiceRequest, Error};
use actix_web::dev::{ServiceResponse, Transform};
use actix_session::SessionExt;
use std::future::{Ready, ready};
use std::task::{Context as TaskContext, Poll};
use actix_web::body::EitherBody;

// Google Sheets API imports (TODO: Fix version conflicts)
// use google_sheets4::{Sheets, api::ValueRange};
// use google_apis_common::oauth2::{ServiceAccountAuthenticator, ServiceAccountKey};
// use hyper::Client;
// use hyper_rustls::HttpsConnectorBuilder;

mod import;
mod gemini_insights;
mod claude_insights;
mod recommendations;
use recommendations::RecommendationRequest;

// Configuration structure
#[derive(Debug, Deserialize, Clone)]
struct Config {
    database_url: String,
    gemini_api_key: String,
    server_host: String,
    server_port: u16,
    excel_file_path: String,
    site_favicon: Option<String>,
    google_client_id: String,
    google_client_secret: String,
    session_key: String,
    allowed_redirect_domains: Vec<String>,
    is_production: bool,
    frontend_url: String,
    // Supabase configuration
    supabase_url: String,
    supabase_service_role_key: String,
    supabase_anon_key: String,
    // LinkedIn OAuth
    linkedin_client_id: Option<String>,
    linkedin_client_secret: Option<String>,
    // GitHub OAuth
    github_client_id: Option<String>,
    github_client_secret: Option<String>,
}

// Thread-safe configuration holder
type SharedConfig = Arc<Mutex<Config>>;

// Rate limiting for auth endpoints
#[derive(Debug)]
struct RateLimiter {
    requests: Arc<Mutex<HashMap<String, Vec<u64>>>>,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn check_rate_limit(&self, ip: &str, max_requests: usize, window_seconds: u64) -> bool {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let mut requests = self.requests.lock().unwrap();
        
        let user_requests = requests.entry(ip.to_string()).or_insert_with(Vec::new);
        
        // Remove old requests outside the window
        user_requests.retain(|&timestamp| now - timestamp < window_seconds);
        
        if user_requests.len() >= max_requests {
            return false; // Rate limit exceeded
        }
        
        user_requests.push(now);
        true
    }
}

// CSRF token storage
type CsrfTokenStore = Arc<Mutex<HashMap<String, (String, u64)>>>;

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        // Try to load from .env file first
        dotenv::dotenv().ok();
        
        // Also check for a config.toml file
        if let Ok(config_str) = std::fs::read_to_string("config.toml") {
            toml::from_str(&config_str).context("Failed to parse config.toml")
        } else {
            // Fall back to environment variables
            let database_url = Self::build_database_url();
            
            Ok(Config {
                database_url,
                gemini_api_key: std::env::var("GEMINI_API_KEY")
                    .unwrap_or_else(|_| "dummy_key".to_string()),
                server_host: std::env::var("SERVER_HOST")
                    .unwrap_or_else(|_| "127.0.0.1".to_string()),
                server_port: std::env::var("SERVER_PORT")
                    .unwrap_or_else(|_| "8081".to_string())
                    .parse()
                    .unwrap_or(8081),
                excel_file_path: std::env::var("EXCEL_FILE_PATH")
                    .unwrap_or_else(|_| "preferences/projects/DFC-ActiveProjects.xlsx".to_string()),
                site_favicon: std::env::var("SITE_FAVICON").ok(),
                google_client_id: std::env::var("GOOGLE_CLIENT_ID")
                    .unwrap_or_else(|_| "your-google-client-id.apps.googleusercontent.com".to_string()),
                google_client_secret: std::env::var("GOOGLE_CLIENT_SECRET")
                    .unwrap_or_else(|_| "your-google-client-secret".to_string()),
                session_key: std::env::var("SESSION_KEY")
                    .unwrap_or_else(|_| "your-32-byte-session-key-here-change-in-production".to_string()),
                allowed_redirect_domains: std::env::var("ALLOWED_REDIRECT_DOMAINS")
                    .unwrap_or_else(|_| "localhost:8887,localhost:8888".to_string())
                    .split(',').map(|s| s.trim().to_string()).collect(),
                is_production: std::env::var("NODE_ENV").unwrap_or_default() == "production",
                frontend_url: std::env::var("FRONTEND_URL")
                    .unwrap_or_else(|_| "http://localhost:8887/team".to_string()),
                // Supabase configuration
                supabase_url: std::env::var("SUPABASE_URL")
                    .unwrap_or_else(|_| "your-supabase-url".to_string()),
                supabase_service_role_key: std::env::var("SUPABASE_JWT_SECRET")
                    .unwrap_or_else(|_| "your-supabase-jwt-secret".to_string()),
                supabase_anon_key: std::env::var("SUPABASE_ANON_KEY")
                    .unwrap_or_else(|_| "your-supabase-anon-key".to_string()),
                // LinkedIn OAuth
                linkedin_client_id: std::env::var("LINKEDIN_CLIENT_ID").ok(),
                linkedin_client_secret: std::env::var("LINKEDIN_CLIENT_SECRET").ok(),
                // GitHub OAuth
                github_client_id: std::env::var("GITHUB_CLIENT_ID").ok(),
                github_client_secret: std::env::var("GITHUB_CLIENT_SECRET").ok(),
            })
        }
    }
    
    fn reload() -> anyhow::Result<Self> {
        log::info!("Reloading configuration from .env file");
        
        // Force reload of .env file by reading it directly and setting env vars
        if let Ok(env_content) = std::fs::read_to_string(".env") {
            for line in env_content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                
                if let Some((key, value)) = line.split_once('=') {
                    let key = key.trim();
                    let value = value.trim();
                    std::env::set_var(key, value);
                }
            }
        }
        
        Self::from_env()
    }
    
    fn build_database_url() -> String {
        // First, try COMMONS component variables (more secure)
        if let (Ok(host), Ok(port), Ok(name), Ok(user), Ok(password)) = (
            std::env::var("COMMONS_HOST"),
            std::env::var("COMMONS_PORT"),
            std::env::var("COMMONS_NAME"),
            std::env::var("COMMONS_USER"),
            std::env::var("COMMONS_PASSWORD")
        ) {
            let ssl_mode = std::env::var("COMMONS_SSL_MODE").unwrap_or_else(|_| "require".to_string());
            format!("postgres://{user}:{password}@{host}:{port}/{name}?sslmode={ssl_mode}")
        } else if let (Ok(host), Ok(port), Ok(name), Ok(user), Ok(password)) = (
            std::env::var("DB_HOST"),
            std::env::var("DB_PORT"),
            std::env::var("DB_NAME"),
            std::env::var("DB_USER"),
            std::env::var("DB_PASSWORD")
        ) {
            // Fall back to generic DB_ variables
            let ssl_mode = std::env::var("DB_SSL_MODE").unwrap_or_else(|_| "require".to_string());
            format!("postgres://{user}:{password}@{host}:{port}/{name}?sslmode={ssl_mode}")
        } else {
            // Fall back to full DATABASE_URL
            std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://user:password@localhost/suitecrm".to_string())
        }
    }
}

// Persistent Claude Session Manager
#[derive(Debug)]
struct ClaudeSession {
    process: Option<Child>,
    session_start: u64,
    prompt_count: u32,
    total_input_tokens: u32,
    total_output_tokens: u32,
    last_usage: Option<serde_json::Value>,
}

impl ClaudeSession {
    fn new() -> Self {
        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
            
        ClaudeSession {
            process: None,
            session_start: start_time,
            prompt_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            last_usage: None,
        }
    }
    
    fn is_active(&self) -> bool {
        self.process.is_some()
    }
    
    fn get_session_duration(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        now - self.session_start
    }
}

type ClaudeSessionManager = Arc<Mutex<ClaudeSession>>;

// CLI structure
#[derive(Parser)]
#[command(name = "suitecrm")]
#[command(about = "SuiteCRM with Gemini AI Integration", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the REST API server
    Serve,
    /// Initialize database schema
    InitDb,
}

// API State
struct ApiState {
    db: Pool<Postgres>,
    config: SharedConfig,
}

// Function to start watching .env file for changes
fn start_env_watcher(config: SharedConfig) -> anyhow::Result<()> {
    use notify::{Event, EventKind};
    
    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(tx, NotifyConfig::default())?;
    
    // Watch the .env file
    let env_path = Path::new(".env");
    if env_path.exists() {
        watcher.watch(env_path, RecursiveMode::NonRecursive)?;
        log::info!("Started watching .env file for changes");
        
        // Spawn a background thread to handle file change events
        let config_clone = config.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv() {
                    Ok(event) => {
                        match event {
                            Ok(Event { kind: EventKind::Modify(_), paths, .. }) |
                            Ok(Event { kind: EventKind::Create(_), paths, .. }) => {
                                if paths.iter().any(|path| path.file_name() == Some(std::ffi::OsStr::new(".env"))) {
                                    log::info!(".env file changed, reloading configuration...");
                                    
                                    // Add a small delay to ensure file write is complete
                                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                                    
                                    match Config::reload() {
                                        Ok(new_config) => {
                                            if let Ok(mut config_guard) = config_clone.lock() {
                                                *config_guard = new_config;
                                                log::info!("Configuration reloaded successfully");
                                            } else {
                                                log::error!("Failed to acquire config lock for reload");
                                            }
                                        }
                                        Err(e) => {
                                            log::error!("Failed to reload configuration: {e}");
                                        }
                                    }
                                }
                            }
                            Ok(Event { kind: EventKind::Remove(_), paths, .. }) => {
                                if paths.iter().any(|path| path.file_name() == Some(std::ffi::OsStr::new(".env"))) {
                                    log::warn!(".env file was removed");
                                }
                            }
                            _ => {} // Ignore other events
                        }
                    }
                    Err(e) => {
                        log::error!("File watcher error: {e}");
                        break;
                    }
                }
            }
        });
        
        // Keep the watcher alive by storing it
        std::mem::forget(watcher);
    } else {
        log::warn!("No .env file found to watch");
    }
    
    Ok(())
}

// Request/Response types for projects
#[derive(Debug, Serialize, Deserialize)]
struct CreateProjectRequest {
    name: String,
    description: Option<String>,
    status: Option<String>,
    estimated_start_date: Option<String>,
    estimated_end_date: Option<String>,
}

// Google Cloud project creation request
#[derive(Debug, Serialize, Deserialize)]
struct CreateGoogleProjectRequest {
    project_id: String,
    user_email: String,
    org_id: Option<String>,
    billing_id: Option<String>,
    service_key: String,
}

// Google OAuth verification request
#[derive(Debug, Serialize, Deserialize)]
struct GoogleAuthRequest {
    credential: String,
}

// Google OAuth verification response
#[derive(Debug, Serialize, Deserialize)]
struct GoogleAuthResponse {
    success: bool,
    name: String,
    email: String,
    picture: Option<String>,
}

// OAuth URL response
#[derive(Debug, Serialize, Deserialize)]
struct OAuthUrlResponse {
    auth_url: String,
    state: String,
}

// OAuth callback request
#[derive(Debug, Serialize, Deserialize)]
struct OAuthCallbackRequest {
    code: String,
    state: Option<String>,
}

// JWT Claims structure
#[derive(Debug, Serialize, Deserialize)]
struct JWTClaims {
    sub: String, // user id
    email: String,
    name: String,
    picture: Option<String>,
    provider: String, // google, github, linkedin
    exp: usize, // expiration timestamp
    iat: usize, // issued at timestamp
}

// Supabase auth response
#[derive(Debug, Serialize, Deserialize)]
struct SupabaseAuthResponse {
    success: bool,
    user: Option<SupabaseUser>,
    session: Option<SupabaseSession>,
    error: Option<String>,
}

// Supabase user structure
#[derive(Debug, Serialize, Deserialize)]
struct SupabaseUser {
    id: String,
    email: String,
    user_metadata: serde_json::Value,
    app_metadata: serde_json::Value,
    created_at: String,
}

// Supabase session structure
#[derive(Debug, Serialize, Deserialize)]
struct SupabaseSession {
    access_token: String,
    token_type: String,
    expires_in: i64,
    refresh_token: String,
    user: SupabaseUser,
}

// User session info
#[derive(Debug, Serialize, Deserialize, Clone)]
struct UserSession {
    user_id: String,
    email: String,
    name: String,
    picture: Option<String>,
    provider: String,
    created_at: i64,
    expires_at: i64,
}

impl UserSession {
    fn is_expired(&self) -> bool {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        now > self.expires_at
    }

    fn new(user_id: String, email: String, name: String, picture: Option<String>) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let expires_at = now + (24 * 60 * 60); // 24 hours from now
        
        Self {
            user_id,
            email,
            name,
            picture,
            provider: "google".to_string(),
            created_at: now,
            expires_at,
        }
    }
}

// Authentication middleware
pub struct AuthMiddleware;

impl<S, B> Transform<S, ServiceRequest> for AuthMiddleware
where
    S: actix_web::dev::Service<
        ServiceRequest,
        Response = ServiceResponse<B>,
        Error = Error,
    >,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = AuthMiddlewareService<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(AuthMiddlewareService { service }))
    }
}

pub struct AuthMiddlewareService<S> {
    service: S,
}

impl<S, B> actix_web::dev::Service<ServiceRequest> for AuthMiddlewareService<S>
where
    S: actix_web::dev::Service<
        ServiceRequest,
        Response = ServiceResponse<B>,
        Error = Error,
    >,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>>>,
    >;

    fn poll_ready(&self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let path = req.path().to_string();
        
        // Skip auth for public endpoints
        if path.starts_with("/api/auth") || 
           path.starts_with("/api/health") || 
           path.starts_with("/api/recommendations") ||
           path.contains("debug") ||
           path.contains("oauth") {
            let fut = self.service.call(req);
            return Box::pin(async move {
                let res = fut.await?;
                Ok(res.map_into_left_body())
            });
        }

        // Check session for protected routes
        let (http_req, payload) = req.into_parts();
        let session = http_req.get_session();
        
        match session.get::<UserSession>("user") {
            Ok(Some(user_session)) if !user_session.is_expired() => {
                // User is authenticated
                let req = ServiceRequest::from_parts(http_req, payload);
                let fut = self.service.call(req);
                Box::pin(async move {
                    let res = fut.await?;
                    Ok(res.map_into_left_body())
                })
            }
            _ => {
                // User is not authenticated or session expired
                let response = HttpResponse::Unauthorized()
                    .json(json!({"error": "Authentication required"}))
                    .map_into_right_body();
                Box::pin(async move {
                    Ok(ServiceResponse::new(http_req, response))
                })
            }
        }
    }
}

// Google user info from token
#[derive(Debug, Serialize, Deserialize)]
struct GoogleUserInfo {
    id: String,
    email: String,
    name: String,
    picture: Option<String>,
    given_name: Option<String>,
    family_name: Option<String>,
}

// Google Sheets member data request
#[derive(Debug, Serialize, Deserialize)]
struct GoogleSheetsMemberRequest {
    data: std::collections::HashMap<String, String>,
    email: String,
    update_existing: bool,
}

#[derive(Debug, Serialize)]
struct TableInfo {
    name: String,
    row_count: i64,
}

#[derive(Serialize)]
struct DatabaseResponse {
    success: bool,
    message: Option<String>,
    error: Option<String>,
    data: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct TableInfoDetailed {
    name: String,
    rows: Option<i64>,
    description: Option<String>,
}

#[derive(Serialize)]
struct ConnectionInfo {
    server_version: String,
    database_name: String,
    current_user: String,
    connection_count: i64,
}

#[derive(Deserialize)]
struct QueryRequest {
    query: String,
}

#[derive(Serialize, Clone)]
struct EnvDatabaseConfig {
    server: String,
    database: String,
    username: String,
    port: u16,
    ssl: bool,
}

#[derive(Serialize)]
struct EnvConfigResponse {
    database: Option<EnvDatabaseConfig>,
    database_connections: Vec<DatabaseConnection>,
    gemini_api_key_present: bool,
    google_project_id: Option<String>,
    google_user_email: Option<String>,
    google_org_id: Option<String>,
    google_billing_id: Option<String>,
    google_service_key: Option<String>,
}

#[derive(Serialize)]
struct DatabaseConnection {
    name: String,
    display_name: String,
    config: EnvDatabaseConfig,
}

#[derive(Deserialize)]
struct SaveEnvConfigRequest {
    #[serde(rename = "GEMINI_API_KEY")]
    gemini_api_key: Option<String>,
    google_project_id: Option<String>,
    google_user_email: Option<String>,
    google_org_id: Option<String>,
    google_billing_id: Option<String>,
    google_service_key: Option<String>,
}

#[derive(Deserialize)]
struct CreateEnvConfigRequest {
    content: String,
}

#[derive(Deserialize)]
struct FetchCsvRequest {
    url: String,
}

// Health check endpoint
async fn health_check(data: web::Data<Arc<ApiState>>) -> Result<HttpResponse> {
    match sqlx::query("SELECT 1").fetch_one(&data.db).await {
        Ok(_) => Ok(HttpResponse::Ok().json(json!({
            "status": "healthy",
            "database_connected": true
        }))),
        Err(e) => Ok(HttpResponse::Ok().json(json!({
            "status": "unhealthy",
            "database_connected": false,
            "error": e.to_string()
        }))),
    }
}

// Get current configuration from shared state
async fn get_current_config(data: web::Data<Arc<ApiState>>) -> Result<HttpResponse> {
    let config_guard = data.config.lock().unwrap();
    let config_json = json!({
        "server_host": config_guard.server_host,
        "server_port": config_guard.server_port,
        "site_favicon": config_guard.site_favicon,
        "gemini_api_key_present": !config_guard.gemini_api_key.is_empty() && config_guard.gemini_api_key != "dummy_key"
    });
    
    Ok(HttpResponse::Ok().json(config_json))
}

// Get environment configuration
async fn get_env_config() -> Result<HttpResponse> {
    let mut database_config = None;
    let mut database_connections = Vec::new();
    
    // Helper function to build config from components
    let build_config_from_components = |prefix: &str| -> Option<(String, EnvDatabaseConfig)> {
        let host_key = format!("{prefix}_HOST");
        let port_key = format!("{prefix}_PORT");
        let name_key = format!("{prefix}_NAME");
        let user_key = format!("{prefix}_USER");
        let password_key = format!("{prefix}_PASSWORD");
        let ssl_key = format!("{prefix}_SSL_MODE");
        
        if let (Ok(host), Ok(port), Ok(name), Ok(user), Ok(_password)) = (
            std::env::var(&host_key),
            std::env::var(&port_key),
            std::env::var(&name_key),
            std::env::var(&user_key),
            std::env::var(&password_key)
        ) {
            let ssl_mode = std::env::var(&ssl_key).unwrap_or_else(|_| "require".to_string());
            let port_num: u16 = port.parse().unwrap_or(5432);
            let ssl = ssl_mode == "require";
            
            let config = EnvDatabaseConfig {
                server: format!("{host}:{port_num}"),
                database: name.clone(),
                username: user.clone(),
                port: port_num,
                ssl,
            };
            
            let display_name = match prefix {
                "COMMONS" => "MemberCommons Database (Default)".to_string(),
                "EXIOBASE" => "EXIOBASE Database".to_string(),
                _ => format!("{} Database", prefix.replace('_', " ")),
            };
            
            Some((display_name, config))
        } else {
            None
        }
    };
    
    // Check for component-based configurations first
    let component_prefixes = ["COMMONS", "EXIOBASE", "DB"];
    for prefix in component_prefixes.iter() {
        if let Some((display_name, config)) = build_config_from_components(prefix) {
            // Set COMMONS as the default database config
            if *prefix == "COMMONS" {
                database_config = Some(config.clone());
            }
            
            database_connections.push(DatabaseConnection {
                name: prefix.to_string(),
                display_name,
                config,
            });
        }
    }
    
    // Scan for all database URLs in environment variables (legacy support)
    for (key, value) in std::env::vars() {
        if key.ends_with("_URL") && value.starts_with("postgres://") {
            if let Ok(url) = Url::parse(&value) {
                let server = format!("{}:{}", 
                    url.host_str().unwrap_or("unknown"), 
                    url.port().unwrap_or(5432)
                );
                let database = url.path().trim_start_matches('/').to_string();
                let username = url.username().to_string();
                let ssl = value.contains("sslmode=require");
                
                let config = EnvDatabaseConfig {
                    server,
                    database,
                    username,
                    port: url.port().unwrap_or(5432),
                    ssl,
                };
                
                // Set the default database (DATABASE_URL) as the main config
                if key == "DATABASE_URL" {
                    database_config = Some(config.clone());
                }
                
                // Add to connections list with display name
                let display_name = match key.as_str() {
                    "DATABASE_URL" => "MemberCommons Database (Default)".to_string(),
                    "EXIOBASE_URL" => "EXIOBASE Database".to_string(),
                    _ => {
                        let name = key.replace("_URL", "").replace("_", " ");
                        format!("{} Database", name.split_whitespace()
                            .map(|word| {
                                let mut chars = word.chars();
                                match chars.next() {
                                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                                    None => String::new(),
                                }
                            })
                            .collect::<Vec<_>>()
                            .join(" "))
                    }
                };
                
                database_connections.push(DatabaseConnection {
                    name: key,
                    display_name,
                    config,
                });
            }
        }
    }
    
    // Check if Gemini API key is present and valid (but don't expose the actual key)
    let gemini_api_key_present = if let Ok(key) = std::env::var("GEMINI_API_KEY") {
        !key.is_empty() && key != "dummy_key" && key != "get-key-at-aistudio.google.com"
    } else {
        false
    };
    
    // Get Google configuration values
    let google_project_id = std::env::var("GOOGLE_PROJECT_ID").ok();
    let google_user_email = std::env::var("GOOGLE_USER_EMAIL").ok();
    let google_org_id = std::env::var("GOOGLE_ORG_ID").ok();
    let google_billing_id = std::env::var("GOOGLE_BILLING_ID").ok();
    let google_service_key = std::env::var("GOOGLE_SERVICE_KEY").ok();
    
    Ok(HttpResponse::Ok().json(EnvConfigResponse {
        database: database_config,
        database_connections,
        gemini_api_key_present,
        google_project_id,
        google_user_email,
        google_org_id,
        google_billing_id,
        google_service_key,
    }))
}

// Restart server endpoint (for development)
async fn restart_server() -> Result<HttpResponse> {
    // In a production environment, you might want to add authentication here
    
    // For development, just exit and let the user restart manually
    // This is safer and more reliable than trying to auto-restart
    tokio::spawn(async {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        std::process::exit(0); // Clean exit
    });
    
    Ok(HttpResponse::Ok().json(json!({
        "message": "Server shutdown initiated. Please restart manually with 'cargo run serve'",
        "status": "success"
    })))
}

// Save environment configuration to .env file
async fn save_env_config(req: web::Json<SaveEnvConfigRequest>) -> Result<HttpResponse> {
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};
    
    let env_path = ".env";
    let mut env_lines = Vec::new();
    let mut updated_keys = std::collections::HashSet::<String>::new();
    
    // Read existing .env file if it exists
    if let Ok(file) = std::fs::File::open(env_path) {
        let reader = BufReader::new(file);
        for line in reader.lines().map_while(Result::ok) {
            env_lines.push(line);
        }
    }
    
    // Helper function to update or add environment variable
    let update_env_var = |env_lines: &mut Vec<String>, updated_keys: &mut std::collections::HashSet<String>, key: &str, value: &Option<String>| {
        if let Some(val) = value {
            if !val.is_empty() {
                let new_line = format!("{key}={val}");
                
                // Find and update existing key, or mark for addition
                let mut found = false;
                for line in env_lines.iter_mut() {
                    // Skip empty lines and comments
                    if line.trim().is_empty() || line.trim().starts_with('#') {
                        continue;
                    }
                    
                    // Check if line starts with the key followed by = (with optional whitespace)
                    let line_trimmed = line.trim();
                    if line_trimmed.starts_with(&format!("{key}=")) || 
                       line_trimmed.starts_with(&format!("{key} =")) {
                        *line = new_line.clone();
                        found = true;
                        break;
                    }
                }
                
                if !found {
                    env_lines.push(new_line);
                }
                updated_keys.insert(key.to_string());
            }
        }
    };
    
    // Update or add new values
    update_env_var(&mut env_lines, &mut updated_keys, "GEMINI_API_KEY", &req.gemini_api_key);
    update_env_var(&mut env_lines, &mut updated_keys, "GOOGLE_PROJECT_ID", &req.google_project_id);
    update_env_var(&mut env_lines, &mut updated_keys, "GOOGLE_USER_EMAIL", &req.google_user_email);
    update_env_var(&mut env_lines, &mut updated_keys, "GOOGLE_ORG_ID", &req.google_org_id);
    update_env_var(&mut env_lines, &mut updated_keys, "GOOGLE_BILLING_ID", &req.google_billing_id);
    update_env_var(&mut env_lines, &mut updated_keys, "GOOGLE_SERVICE_KEY", &req.google_service_key);
    
    // Write back to .env file
    match OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(env_path)
    {
        Ok(mut file) => {
            for line in env_lines {
                writeln!(file, "{line}").map_err(|e| {
                    actix_web::error::ErrorInternalServerError(format!("Failed to write to .env file: {e}"))
                })?;
            }
            
            // Update environment variables in current process
            let set_env_var = |key: &str, value: &Option<String>| {
                if let Some(val) = value {
                    if !val.is_empty() {
                        std::env::set_var(key, val);
                    }
                }
            };
            
            set_env_var("GEMINI_API_KEY", &req.gemini_api_key);
            set_env_var("GOOGLE_PROJECT_ID", &req.google_project_id);
            set_env_var("GOOGLE_USER_EMAIL", &req.google_user_email);
            set_env_var("GOOGLE_ORG_ID", &req.google_org_id);
            set_env_var("GOOGLE_BILLING_ID", &req.google_billing_id);
            set_env_var("GOOGLE_SERVICE_KEY", &req.google_service_key);
            
            Ok(HttpResponse::Ok().json(json!({
                "success": true,
                "message": "Configuration saved to .env file",
                "updated_keys": updated_keys.into_iter().collect::<Vec<_>>()
            })))
        }
        Err(e) => {
            Ok(HttpResponse::InternalServerError().json(json!({
                "success": false,
                "error": format!("Failed to write .env file: {e}")
            })))
        }
    }
}

// Create .env file from .env.example content
async fn create_env_config(req: web::Json<CreateEnvConfigRequest>) -> Result<HttpResponse> {
    use std::fs;
    
    // Check if .env file already exists
    if std::path::Path::new(".env").exists() {
        return Ok(HttpResponse::BadRequest().json(json!({
            "success": false,
            "error": ".env file already exists"
        })));
    }
    
    // Write the content to .env file
    match fs::write(".env", &req.content) {
        Ok(_) => {
            Ok(HttpResponse::Ok().json(json!({
                "success": true,
                "message": ".env file created successfully from .env.example template"
            })))
        }
        Err(e) => {
            Ok(HttpResponse::InternalServerError().json(json!({
                "success": false,
                "error": format!("Failed to create .env file: {e}")
            })))
        }
    }
}

// Create Google Cloud project via API
async fn create_google_project(req: web::Json<CreateGoogleProjectRequest>) -> Result<HttpResponse> {
    // Validate required fields
    if req.project_id.is_empty() {
        return Ok(HttpResponse::BadRequest().json(json!({
            "success": false,
            "error": "Project ID is required"
        })));
    }
    
    if req.user_email.is_empty() {
        return Ok(HttpResponse::BadRequest().json(json!({
            "success": false,
            "error": "User email is required"
        })));
    }
    
    if req.service_key.is_empty() {
        return Ok(HttpResponse::BadRequest().json(json!({
            "success": false,
            "error": "Service account key is required for API access"
        })));
    }
    
    // Validate service key is valid JSON
    if let Err(_) = serde_json::from_str::<serde_json::Value>(&req.service_key) {
        return Ok(HttpResponse::BadRequest().json(json!({
            "success": false,
            "error": "Service account key must be valid JSON",
            "help": {
                "title": "How to Get Your Google Service Account Key",
                "style": "info", // This will trigger light blue background in frontend
                "google_console_url": "https://console.cloud.google.com/iam-admin/serviceaccounts",
                "steps": [
                    "1. Go to Google Cloud Console → IAM & Admin → Service Accounts",
                    "2. Click 'Create Service Account' or select existing one", 
                    "3. Grant 'Cloud Resource Manager Admin' role (required for project creation)",
                    "4. Click 'Keys' tab → 'Add Key' → 'Create New Key'",
                    "5. Choose 'JSON' format and download the file",
                    "6. Copy the entire JSON content into the 'Service Account Key' field above"
                ],
                "billing_info": {
                    "required_for": "Creating new Google Cloud projects via API",
                    "not_required_for": "Accessing Google Meet/Calendar APIs on existing projects",
                    "note": "For Google Meetup participant feeds, billing is typically not required unless you exceed free tier limits"
                },
                "json_format_example": "Should start with: {\"type\":\"service_account\",\"project_id\":\"...\",\"private_key_id\":\"...\"}"
            }
        })));
    }
    
    // For now, return a placeholder response indicating the feature is not fully implemented
    // In a real implementation, this would:
    // 1. Parse the service account key
    // 2. Authenticate with Google Cloud Resource Manager API
    // 3. Create the project using the Google Cloud API
    // 4. Set up billing if billing_id is provided
    // 5. Add the user email to the project IAM
    
    Ok(HttpResponse::Ok().json(json!({
        "success": false,
        "error": "Google Cloud Project API integration is not yet implemented. Please use the manual method for now.",
        "message": "To manually create the project, click 'Via Google Page' and follow the instructions.",
        "troubleshooting": {
            "manual_steps": [
                "1. Click 'Via Google Page' button",
                "2. Follow the Google Cloud Console instructions",
                "3. Use the provided project ID and billing information",
                "4. Return here and click 'Project Created' when done"
            ],
            "api_implementation_needed": [
                "Google Cloud Resource Manager API integration",
                "Service account authentication",
                "Project creation and billing setup",
                "IAM role assignment"
            ]
        }
    })))
}

// Google OAuth verification handler
// Get redirect URI with intelligent defaults
fn get_redirect_uri(config: &Config) -> String {
    // Check if custom redirect URI is set
    if let Ok(custom_uri) = std::env::var("GOOGLE_REDIRECT_URI") {
        return custom_uri;
    }
    
    // Handle localhost vs 127.0.0.1 variations
    let host = if config.server_host == "127.0.0.1" || config.server_host == "localhost" {
        "localhost"  // Google prefers localhost over 127.0.0.1
    } else {
        &config.server_host
    };
    
    format!("http://{}:{}/api/auth/google/callback", host, config.server_port)
}

fn create_oauth_client(config: &Config) -> BasicClient {
    BasicClient::new(
        ClientId::new(config.google_client_id.clone()),
        Some(ClientSecret::new(config.google_client_secret.clone())),
        AuthUrl::new("https://accounts.google.com/o/oauth2/v2/auth".to_string()).unwrap(),
        Some(TokenUrl::new("https://oauth2.googleapis.com/token".to_string()).unwrap()),
    )
    .set_redirect_uri(RedirectUrl::new(get_redirect_uri(&config)).unwrap())
}

async fn google_auth_url(data: web::Data<SharedConfig>) -> Result<HttpResponse> {
    
    let config = data.lock().unwrap();
    
    // Check if Google credentials are configured (don't expose debug info in production)
    if config.google_client_id.contains("your-google-client-id") {
        let response = if config.is_production {
            json!({
                "error": "Authentication service temporarily unavailable",
                "message": "Please try again later"
            })
        } else {
            json!({
                "error": "Google OAuth not configured",
                "message": "Please set GOOGLE_CLIENT_ID and GOOGLE_CLIENT_SECRET in your .env file",
                "debug_info": {
                    "client_id_configured": false,
                    "redirect_uri": get_redirect_uri(&config)
                }
            })
        };
        return Ok(HttpResponse::ServiceUnavailable().json(response));
    }
    
    if config.google_client_secret.contains("your-google-client-secret") {
        let response = if config.is_production {
            json!({
                "error": "Authentication service temporarily unavailable",
                "message": "Please try again later"
            })
        } else {
            json!({
                "error": "Google OAuth client secret not configured", 
                "message": "Please set GOOGLE_CLIENT_SECRET in your .env file"
            })
        };
        return Ok(HttpResponse::ServiceUnavailable().json(response));
    }
    
    let client = create_oauth_client(&config);
    
    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()))
        .add_scope(Scope::new("openid".to_string()))
        .url();
    
    // For now, simplified CSRF handling (state is validated by OAuth2 lib)
    
    log::info!("Generated OAuth URL for redirect_uri: {}", get_redirect_uri(&config));
    
    Ok(HttpResponse::Ok().json(OAuthUrlResponse {
        auth_url: auth_url.to_string(),
        state: csrf_token.secret().clone(),
    }))
}

// Helper function to validate redirect URL against whitelist
fn validate_redirect_url(url: &str, config: &Config) -> bool {
    if let Ok(parsed_url) = Url::parse(url) {
        if let Some(host) = parsed_url.host_str() {
            let host_with_port = if let Some(port) = parsed_url.port() {
                format!("{}:{}", host, port)
            } else {
                host.to_string()
            };
            
            return config.allowed_redirect_domains.iter()
                .any(|domain| domain == &host_with_port || domain == host);
        }
    }
    false
}

// Helper function to create redirect response
fn create_auth_redirect(success: bool, message: Option<&str>, config: &Config) -> HttpResponse {
    // Use configurable frontend URL from environment
    let redirect_url = if success {
        format!("{}/?auth=success#account/preferences", config.frontend_url)
    } else {
        let msg = message.unwrap_or("unknown_error");
        format!("{}/?auth=error&message={}", config.frontend_url, msg)
    };
    
    // Validate redirect URL
    if !validate_redirect_url(&redirect_url, config) {
        log::error!("Invalid redirect URL: {}", redirect_url);
        return HttpResponse::BadRequest().json(json!({
            "error": "Invalid redirect URL"
        }));
    }
    
    log::info!("Redirecting to: {}", redirect_url);
    
    HttpResponse::Found()
        .append_header(("Location", redirect_url))
        .finish()
}

async fn google_auth_callback(
    query: web::Query<OAuthCallbackRequest>,
    session: Session,
    data: web::Data<SharedConfig>,
) -> Result<HttpResponse> {
    log::info!("OAuth callback received with code: {}", &query.code[..10]);
    
    let config = data.lock().unwrap();
    
    // Check if Google credentials are configured
    if config.google_client_id.contains("your-google-client-id") {
        log::error!("Google OAuth not configured - using demo user");
        return Ok(create_auth_redirect(false, Some("oauth_not_configured"), &config));
    }
    
    // Create OAuth client
    let client = create_oauth_client(&config);
    drop(config); // Release the lock
    
    // Exchange the authorization code for an access token
    let token_response = match client
        .exchange_code(AuthorizationCode::new(query.code.clone()))
        .request_async(async_http_client)
        .await
    {
        Ok(token) => token,
        Err(e) => {
            log::error!("Failed to exchange code for token: {:?}", e);
            let config = data.lock().unwrap();
            return Ok(create_auth_redirect(false, Some("token_exchange_failed"), &config));
        }
    };
    
    // Use the access token to get user information from Google
    let user_info_url = format!(
        "https://www.googleapis.com/oauth2/v2/userinfo?access_token={}",
        token_response.access_token().secret()
    );
    
    let user_info: GoogleUserInfo = match reqwest::get(&user_info_url).await {
        Ok(response) => {
            if !response.status().is_success() {
                log::error!("Failed to get user info, status: {}", response.status());
                let config = data.lock().unwrap();
                return Ok(create_auth_redirect(false, Some("user_info_failed"), &config));
            }
            match response.json().await {
                Ok(info) => info,
                Err(e) => {
                    log::error!("Failed to parse user info JSON: {:?}", e);
                    let config = data.lock().unwrap();
                    return Ok(create_auth_redirect(false, Some("user_info_parse_failed"), &config));
                }
            }
        }
        Err(e) => {
            log::error!("Failed to fetch user info: {:?}", e);
            let config = data.lock().unwrap();
            return Ok(create_auth_redirect(false, Some("network_error"), &config));
        }
    };
    
    log::info!("Successfully retrieved user info for: {}", user_info.email);
    
    // Create a UserSession object with real user data
    let user_session = UserSession::new(
        user_info.id.clone(),
        user_info.email.clone(),
        user_info.name.clone(),
        user_info.picture.clone(),
    );
    
    // Store user session
    if let Err(e) = session.insert("user", &user_session) {
        log::error!("Failed to store user session: {:?}", e);
        let config = data.lock().unwrap();
        return Ok(create_auth_redirect(false, Some("session_store_failed"), &config));
    }
    
    log::info!("User session created successfully for: {}", user_info.email);
    
    // Redirect back to frontend with auth success parameter
    let config = data.lock().unwrap();
    Ok(create_auth_redirect(true, None, &config))
}

async fn ensure_user_exists(pool: &Pool<Postgres>, user_info: &GoogleUserInfo) -> anyhow::Result<String> {
    // First, find user by checking email addresses table and relationships
    let existing_user = sqlx::query(
        r#"
        SELECT u.id FROM users u
        JOIN email_addr_bean_rel eabr ON u.id = eabr.bean_id
        JOIN email_addresses ea ON eabr.email_address_id = ea.id
        WHERE ea.email_address = $1 AND eabr.bean_module = 'Users' AND eabr.deleted = 0
        "#
    )
    .bind(&user_info.email)
    .fetch_optional(pool)
    .await?;
    
    if let Some(row) = existing_user {
        return Ok(row.try_get::<Uuid, _>("id")?.to_string());
    }
    
    // Create new user and email relationship
    let user_id = Uuid::new_v4();
    let email_id = Uuid::new_v4();
    let now = Utc::now();
    
    // Start transaction
    let mut tx = pool.begin().await?;
    
    // Insert user
    sqlx::query(
        r#"INSERT INTO users (id, first_name, last_name, user_name,
           date_entered, date_modified, created_by, modified_user_id, status, deleted)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'Active', false)"#
    )
    .bind(&user_id)
    .bind(user_info.given_name.as_deref().unwrap_or(""))
    .bind(user_info.family_name.as_deref().unwrap_or(""))
    .bind(&user_info.email) // Use email as username
    .bind(&now)
    .bind(&now)
    .bind(&user_id)
    .bind(&user_id)
    .execute(&mut *tx)
    .await?;
    
    // Insert email address
    sqlx::query(
        r#"INSERT INTO email_addresses (id, email_address, date_created, date_modified)
           VALUES ($1, $2, $3, $4)"#
    )
    .bind(&email_id)
    .bind(&user_info.email)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    
    // Link email to user
    sqlx::query(
        r#"INSERT INTO email_addr_bean_rel (id, email_address_id, bean_id, bean_module, primary_address, deleted)
           VALUES (uuid_generate_v4(), $1, $2, 'Users', true, false)"#
    )
    .bind(&email_id)
    .bind(&user_id)
    .execute(&mut *tx)
    .await?;
    
    tx.commit().await?;
    
    Ok(user_id.to_string())
}

async fn get_current_user(session: Session, req: actix_web::HttpRequest) -> Result<HttpResponse> {
    log::info!("Checking user session...");
    
    // Log all cookies for debugging
    let cookies: Vec<String> = req.headers()
        .get_all("cookie")
        .map(|h| h.to_str().unwrap_or("invalid").to_string())
        .collect();
    log::info!("Received cookies: {:?}", cookies);
    
    match session.get::<UserSession>("user")? {
        Some(user) => {
            // Check if session has expired
            if user.is_expired() {
                log::info!("User session expired for: {}", user.email);
                session.clear();
                Ok(HttpResponse::Ok().json(json!({"success": false, "error": "Session expired"})))
            } else {
                log::info!("Found valid user session: {}", user.email);
                Ok(HttpResponse::Ok().json(json!({"success": true, "user": user})))
            }
        },
        None => {
            log::warn!("No user session found in session storage");
            Ok(HttpResponse::Ok().json(json!({"success": false, "error": "Not authenticated"})))
        },
    }
}

// Debug endpoint to check session status
async fn debug_session(session: Session) -> Result<HttpResponse> {
    let session_data: Option<UserSession> = session.get("user").unwrap_or(None);
    
    Ok(HttpResponse::Ok().json(json!({
        "session_exists": session_data.is_some(),
        "session_data": session_data,
        "debug": "This endpoint helps debug session issues"
    })))
}

async fn logout_user(session: Session) -> Result<HttpResponse> {
    session.clear();
    Ok(HttpResponse::Ok().json(json!({"success": true})))
}

async fn debug_oauth_config(data: web::Data<SharedConfig>) -> Result<HttpResponse> {
    let config = data.lock().unwrap();
    let redirect_uri = get_redirect_uri(&config);
    
    let client_id_configured = !config.google_client_id.contains("your-google-client-id");
    let client_secret_configured = !config.google_client_secret.contains("your-google-client-secret");
    let session_key_configured = !config.session_key.contains("your-32-byte-session-key");
    
    Ok(HttpResponse::Ok().json(json!({
        "server_host": config.server_host,
        "server_port": config.server_port,
        "redirect_uri": redirect_uri,
        "configuration": {
            "client_id_configured": client_id_configured,
            "client_secret_configured": client_secret_configured,
            "session_key_configured": session_key_configured,
            "all_configured": client_id_configured && client_secret_configured && session_key_configured
        },
        "environment": {
            "custom_redirect_uri": std::env::var("GOOGLE_REDIRECT_URI").ok()
        },
        "instructions": {
            "setup": "1. Copy .env.example to .env, 2. Set Google credentials, 3. Restart server",
            "test_url": "/api/auth/google/url",
            "google_console": "https://console.cloud.google.com/apis/credentials"
        }
    })))
}

async fn verify_google_auth(_req: web::Json<GoogleAuthRequest>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(json!({
        "success": false,
        "error": "Deprecated endpoint. Use OAuth flow instead.",
        "oauth_endpoints": {
            "start_auth": "/api/auth/google/url",
            "callback": "/api/auth/google/callback", 
            "current_user": "/api/auth/user",
            "logout": "/api/auth/logout"
        }
    })))
}

// LinkedIn OAuth handlers
async fn linkedin_auth_url(data: web::Data<SharedConfig>) -> Result<HttpResponse> {
    let config = match data.lock() {
        Ok(config) => config,
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": "Authentication service temporarily unavailable",
            })));
        }
    };

    let client_id = match &config.linkedin_client_id {
        Some(id) if !id.contains("your-linkedin-client-id") => id,
        _ => {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "LinkedIn OAuth not configured",
            })));
        }
    };

    let client_secret = match &config.linkedin_client_secret {
        Some(secret) if !secret.contains("your-linkedin-client-secret") => secret,
        _ => {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "LinkedIn OAuth client secret not configured",
            })));
        }
    };

    let redirect_uri = format!("http://{}:{}/api/auth/linkedin/callback", config.server_host, config.server_port);
    
    let client = BasicClient::new(
        ClientId::new(client_id.clone()),
        Some(ClientSecret::new(client_secret.clone())),
        AuthUrl::new("https://www.linkedin.com/oauth/v2/authorization".to_string()).unwrap(),
        Some(TokenUrl::new("https://www.linkedin.com/oauth/v2/accessToken".to_string()).unwrap()),
    )
    .set_redirect_uri(RedirectUrl::new(redirect_uri).unwrap());

    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("r_liteprofile".to_string()))
        .add_scope(Scope::new("r_emailaddress".to_string()))
        .url();

    Ok(HttpResponse::Ok().json(OAuthUrlResponse {
        auth_url: auth_url.to_string(),
        state: csrf_token.secret().clone(),
    }))
}

async fn linkedin_auth_callback(
    _query: web::Query<OAuthCallbackRequest>,
    data: web::Data<SharedConfig>,
    session: Session,
) -> Result<HttpResponse> {
    let config = data.lock().unwrap();
    
    if config.linkedin_client_id.is_none() || config.linkedin_client_secret.is_none() {
        return Ok(create_auth_redirect(false, Some("linkedin_not_configured"), &config));
    }

    // For now, return demo user until full LinkedIn implementation
    let user_session = UserSession {
        user_id: "linkedin_demo_user".to_string(),
        email: "demo@linkedin.com".to_string(),
        name: "LinkedIn Demo User".to_string(),
        picture: None,
        provider: "linkedin".to_string(),
        created_at: chrono::Utc::now().timestamp(),
        expires_at: chrono::Utc::now().timestamp() + 3600, // 1 hour
    };

    session.insert("user", &user_session).unwrap_or_default();
    Ok(create_auth_redirect(true, None, &config))
}

// GitHub OAuth handlers
async fn github_auth_url(data: web::Data<SharedConfig>) -> Result<HttpResponse> {
    let config = match data.lock() {
        Ok(config) => config,
        Err(_) => {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": "Authentication service temporarily unavailable",
            })));
        }
    };

    let client_id = match &config.github_client_id {
        Some(id) if !id.contains("your-github-client-id") => id,
        _ => {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "GitHub OAuth not configured",
            })));
        }
    };

    let client_secret = match &config.github_client_secret {
        Some(secret) if !secret.contains("your-github-client-secret") => secret,
        _ => {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "GitHub OAuth client secret not configured",
            })));
        }
    };

    let redirect_uri = format!("http://{}:{}/api/auth/github/callback", config.server_host, config.server_port);
    
    let client = BasicClient::new(
        ClientId::new(client_id.clone()),
        Some(ClientSecret::new(client_secret.clone())),
        AuthUrl::new("https://github.com/login/oauth/authorize".to_string()).unwrap(),
        Some(TokenUrl::new("https://github.com/login/oauth/access_token".to_string()).unwrap()),
    )
    .set_redirect_uri(RedirectUrl::new(redirect_uri).unwrap());

    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("user:email".to_string()))
        .url();

    Ok(HttpResponse::Ok().json(OAuthUrlResponse {
        auth_url: auth_url.to_string(),
        state: csrf_token.secret().clone(),
    }))
}

async fn github_auth_callback(
    _query: web::Query<OAuthCallbackRequest>,
    data: web::Data<SharedConfig>,
    session: Session,
) -> Result<HttpResponse> {
    let config = data.lock().unwrap();
    
    if config.github_client_id.is_none() || config.github_client_secret.is_none() {
        return Ok(create_auth_redirect(false, Some("github_not_configured"), &config));
    }

    // For now, return demo user until full GitHub implementation
    let user_session = UserSession {
        user_id: "github_demo_user".to_string(),
        email: "demo@github.com".to_string(),
        name: "GitHub Demo User".to_string(),
        picture: None,
        provider: "github".to_string(),
        created_at: chrono::Utc::now().timestamp(),
        expires_at: chrono::Utc::now().timestamp() + 3600, // 1 hour
    };

    session.insert("user", &user_session).unwrap_or_default();
    Ok(create_auth_redirect(true, None, &config))
}

// Supabase integration handlers
#[derive(Debug, Serialize, Deserialize)]
struct SupabaseTokenRequest {
    access_token: String,
    provider: String,
}

async fn verify_supabase_token(
    req: web::Json<SupabaseTokenRequest>,
    data: web::Data<SharedConfig>,
    session: Session,
) -> Result<HttpResponse> {
    let config = data.lock().unwrap();
    
    // Verify the Supabase JWT token using the JWT secret
    let validation = Validation::new(Algorithm::HS256);
    let decoding_key = DecodingKey::from_secret(config.supabase_service_role_key.as_bytes());
    
    match decode::<JWTClaims>(&req.access_token, &decoding_key, &validation) {
        Ok(token_data) => {
            let user_session = UserSession {
                user_id: token_data.claims.sub.clone(),
                email: token_data.claims.email.clone(),
                name: token_data.claims.name.clone(),
                picture: token_data.claims.picture.clone(),
                provider: req.provider.clone(),
                created_at: chrono::Utc::now().timestamp(),
                expires_at: token_data.claims.exp as i64,
            };

            session.insert("user", &user_session).unwrap_or_default();
            
            Ok(HttpResponse::Ok().json(json!({
                "success": true,
                "user": user_session
            })))
        }
        Err(e) => {
            log::error!("JWT verification failed: {:?}", e);
            Ok(HttpResponse::Unauthorized().json(json!({
                "success": false,
                "error": "Invalid token"
            })))
        }
    }
}

async fn create_supabase_session(
    req: web::Json<SupabaseSession>,
    session: Session,
) -> Result<HttpResponse> {
    let user_session = UserSession {
        user_id: req.user.id.clone(),
        email: req.user.email.clone(),
        name: req.user.user_metadata.get("full_name")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string(),
        picture: req.user.user_metadata.get("avatar_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        provider: "supabase".to_string(),
        created_at: chrono::Utc::now().timestamp(),
        expires_at: chrono::Utc::now().timestamp() + req.expires_in,
    };

    session.insert("user", &user_session).unwrap_or_default();
    
    Ok(HttpResponse::Ok().json(json!({
        "success": true,
        "user": user_session
    })))
}

// Google Sheets Helper Functions (Placeholder implementations)
// TODO: Complete the Google Sheets API integration by resolving dependency version conflicts

async fn get_sheets_config_data() -> anyhow::Result<serde_json::Value> {
    let config_path = "admin/google/form/config.json";
    let config_content = std::fs::read_to_string(config_path)
        .context("Failed to read sheets config file")?;
    
    let config: serde_json::Value = serde_json::from_str(&config_content)
        .context("Failed to parse sheets config JSON")?;
    
    Ok(config)
}

// Placeholder function - TODO: Implement with actual Google Sheets API
async fn validate_sheets_credentials() -> anyhow::Result<bool> {
    // Check if service account key exists and is valid JSON
    let service_key_json = std::env::var("GOOGLE_SERVICE_KEY")
        .context("GOOGLE_SERVICE_KEY not found in environment")?;
    
    // Try to parse as JSON to validate format
    let _service_account_key: serde_json::Value = serde_json::from_str(&service_key_json)
        .context("Failed to parse service account key JSON")?;
    
    // TODO: Actually validate credentials with Google API
    Ok(true)
}

// Get Google Sheets configuration
async fn get_sheets_config() -> Result<HttpResponse> {
    // Try to read configuration from file
    let config_path = "admin/google/form/config.json";
    
    match std::fs::read_to_string(config_path) {
        Ok(config_content) => {
            match serde_json::from_str::<serde_json::Value>(&config_content) {
                Ok(config) => {
                    Ok(HttpResponse::Ok().json(json!({
                        "success": true,
                        "config": config
                    })))
                }
                Err(e) => {
                    Ok(HttpResponse::InternalServerError().json(json!({
                        "success": false,
                        "error": format!("Failed to parse configuration: {}", e)
                    })))
                }
            }
        }
        Err(_) => {
            // Return default configuration
            Ok(HttpResponse::Ok().json(json!({
                "success": true,
                "config": {
                    "googleSheets": {
                        "spreadsheetId": "REPLACE_WITH_YOUR_GOOGLE_SHEET_ID",
                        "worksheetName": "Members",
                        "headerRow": 1,
                        "dataStartRow": 2
                    },
                    "oauth": {
                        "clientId": "REPLACE_WITH_YOUR_GOOGLE_OAUTH_CLIENT_ID"
                    },
                    "appearance": {
                        "title": "Member Registration",
                        "subtitle": "Join our community of developers and contributors working on sustainable impact projects",
                        "primaryColor": "#3B82F6",
                        "accentColor": "#10B981"
                    },
                    "messages": {
                        "welcomeNew": "Welcome! Please fill out the registration form to join our community of developers working on sustainable impact projects.",
                        "welcomeReturning": "Welcome back! Your existing information has been loaded. Please review and update any details as needed."
                    },
                    "behavior": {
                        "allowDuplicates": false,
                        "requireGithub": true,
                        "showProgress": true,
                        "enablePreview": true
                    },
                    "links": {
                        "membersPage": "https://model.earth/community/members",
                        "projectsPage": "https://model.earth/projects"
                    },
                    "message": "Default configuration loaded. Please update config.json with your Google Sheets details."
                }
            })))
        }
    }
}

// Save Google Sheets configuration
async fn save_sheets_config(req: web::Json<serde_json::Value>) -> Result<HttpResponse> {
    let config_path = "admin/google/form/config.json";
    
    // Create directory if it doesn't exist
    if let Some(parent) = std::path::Path::new(config_path).parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "success": false,
                "error": format!("Failed to create config directory: {}", e)
            })));
        }
    }
    
    // Pretty print the JSON configuration
    match serde_json::to_string_pretty(&*req) {
        Ok(config_json) => {
            match std::fs::write(config_path, config_json) {
                Ok(_) => {
                    Ok(HttpResponse::Ok().json(json!({
                        "success": true,
                        "message": "Form configuration saved successfully to config.json"
                    })))
                }
                Err(e) => {
                    Ok(HttpResponse::InternalServerError().json(json!({
                        "success": false,
                        "error": format!("Failed to write configuration file: {}", e)
                    })))
                }
            }
        }
        Err(e) => {
            Ok(HttpResponse::BadRequest().json(json!({
                "success": false,
                "error": format!("Invalid JSON configuration: {}", e)
            })))
        }
    }
}

// Get member data by email from Google Sheets
async fn get_member_by_email(path: web::Path<String>) -> Result<HttpResponse> {
    let email = path.into_inner();
    
    // Get configuration
    let config = match get_sheets_config_data().await {
        Ok(config) => config,
        Err(e) => {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "success": false,
                "error": format!("Failed to load sheets configuration: {}", e),
                "email": email
            })));
        }
    };
    
    // Extract sheet details from config
    let spreadsheet_id = config["googleSheets"]["spreadsheetId"]
        .as_str()
        .unwrap_or("REPLACE_WITH_YOUR_GOOGLE_SHEET_ID");
    
    if spreadsheet_id == "REPLACE_WITH_YOUR_GOOGLE_SHEET_ID" {
        return Ok(HttpResponse::BadRequest().json(json!({
            "success": false,
            "error": "Google Sheets not configured. Please update spreadsheetId in config.json",
            "email": email,
            "setup_required": {
                "steps": [
                    "1. Create a Google Sheet with member data",
                    "2. Add the spreadsheet ID to admin/google/form/config.json",
                    "3. Add your Google Service Account Key to .env as GOOGLE_SERVICE_KEY",
                    "4. The backend will automatically connect to your sheet"
                ],
                "config_file": "admin/google/form/config.json",
                "env_variable": "GOOGLE_SERVICE_KEY"
            }
        })));
    }
    
    // Check if credentials are configured
    match validate_sheets_credentials().await {
        Ok(_) => {
            // TODO: Replace with actual Google Sheets API call
            // For now, return a message indicating the integration is ready but not fully implemented
            Ok(HttpResponse::Ok().json(json!({
                "success": false,
                "error": "Google Sheets API integration ready but not fully implemented",
                "email": email,
                "message": "Configuration validated. Waiting for Google Sheets API implementation to complete.",
                "status": "credentials_valid_api_pending",
                "next_steps": [
                    "Resolve Google API dependency version conflicts",
                    "Complete the find_member_row_by_email implementation",
                    "Test with real Google Sheets data"
                ]
            })))
        }
        Err(e) => {
            return Ok(HttpResponse::BadRequest().json(json!({
                "success": false,
                "error": format!("Google Sheets credentials invalid: {}", e),
                "email": email,
                "setup_required": {
                    "env_variable": "GOOGLE_SERVICE_KEY",
                    "format": "Valid JSON service account key from Google Cloud Console"
                }
            })));
        }
    }
}

// Create or update member data in Google Sheets
async fn save_member_data(req: web::Json<GoogleSheetsMemberRequest>) -> Result<HttpResponse> {
    // Get configuration
    let config = match get_sheets_config_data().await {
        Ok(config) => config,
        Err(e) => {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "success": false,
                "error": format!("Failed to load sheets configuration: {}", e),
                "email": req.email
            })));
        }
    };
    
    // Extract sheet details from config
    let spreadsheet_id = config["googleSheets"]["spreadsheetId"]
        .as_str()
        .unwrap_or("REPLACE_WITH_YOUR_GOOGLE_SHEET_ID");
    
    if spreadsheet_id == "REPLACE_WITH_YOUR_GOOGLE_SHEET_ID" {
        return Ok(HttpResponse::BadRequest().json(json!({
            "success": false,
            "error": "Google Sheets not configured. Please update spreadsheetId in config.json",
            "email": req.email,
            "setup_required": {
                "steps": [
                    "1. Create a Google Sheet with member data columns",
                    "2. Add the spreadsheet ID to admin/google/form/config.json",
                    "3. Add your Google Service Account Key to .env as GOOGLE_SERVICE_KEY",
                    "4. The backend will automatically save data to your sheet"
                ],
                "config_file": "admin/google/form/config.json",
                "env_variable": "GOOGLE_SERVICE_KEY"
            }
        })));
    }
    
    // Check if credentials are configured
    match validate_sheets_credentials().await {
        Ok(_) => {
            // TODO: Replace with actual Google Sheets API call
            // For now, simulate success to allow form testing
            Ok(HttpResponse::Ok().json(json!({
                "success": false,
                "error": "Google Sheets API integration ready but not fully implemented",
                "email": req.email,
                "update_existing": req.update_existing,
                "message": "Form data received and validated. Google Sheets integration pending.",
                "status": "credentials_valid_api_pending",
                "data_received": {
                    "fields_count": req.data.len(),
                    "sample_fields": req.data.keys().take(5).collect::<Vec<_>>(),
                    "operation": if req.update_existing { "update" } else { "create" }
                },
                "next_steps": [
                    "Resolve Google API dependency version conflicts",
                    "Complete the append_member_row/update_member_row implementations",
                    "Test with real Google Sheets data"
                ]
            })))
        }
        Err(e) => {
            return Ok(HttpResponse::BadRequest().json(json!({
                "success": false,
                "error": format!("Google Sheets credentials invalid: {}", e),
                "email": req.email,
                "setup_required": {
                    "env_variable": "GOOGLE_SERVICE_KEY",
                    "format": "Valid JSON service account key from Google Cloud Console"
                }
            })));
        }
    }
}

// Fetch CSV data from external URL (proxy for CORS)
async fn fetch_csv(req: web::Json<FetchCsvRequest>) -> Result<HttpResponse> {
    let url = &req.url;
    
    // Validate URL is from Google Sheets
    if !url.contains("docs.google.com/spreadsheets") {
        return Ok(HttpResponse::BadRequest().json(json!({
            "success": false,
            "error": "Only Google Sheets URLs are allowed"
        })));
    }
    
    match reqwest::get(url).await {
        Ok(response) => {
            if response.status().is_success() {
                match response.text().await {
                    Ok(csv_data) => {
                        if csv_data.trim().is_empty() {
                            Ok(HttpResponse::Ok().json(json!({
                                "success": false,
                                "error": "The spreadsheet appears to be empty or not publicly accessible"
                            })))
                        } else {
                            Ok(HttpResponse::Ok().json(json!({
                                "success": true,
                                "data": csv_data
                            })))
                        }
                    }
                    Err(e) => {
                        Ok(HttpResponse::Ok().json(json!({
                            "success": false,
                            "error": format!("Failed to read response data: {e}")
                        })))
                    }
                }
            } else {
                Ok(HttpResponse::Ok().json(json!({
                    "success": false,
                    "error": format!("HTTP {}: The spreadsheet may not be publicly accessible or the URL is incorrect", response.status())
                })))
            }
        }
        Err(e) => {
            Ok(HttpResponse::Ok().json(json!({
                "success": false,
                "error": format!("Network error: {e}")
            })))
        }
    }
}





#[derive(Debug, Deserialize)]
struct ProxyRequest {
    url: String,
    method: Option<String>,
    headers: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Serialize)]
struct ProxyResponse {
    success: bool,
    data: Option<serde_json::Value>,
    error: Option<String>,
}





// Analyze data with Claude Code CLI
async fn get_recommendations_handler(req: web::Json<RecommendationRequest>, data: web::Data<Arc<ApiState>>) -> Result<HttpResponse> {
    let excel_file_path = {
        let config_guard = data.config.lock().unwrap();
        config_guard.excel_file_path.clone()
    };
    match recommendations::get_recommendations(&req.preferences, &excel_file_path) {
        Ok(projects) => Ok(HttpResponse::Ok().json(projects)),
        Err(e) => Ok(HttpResponse::InternalServerError().json(json!({ "error": e.to_string() }))),
    }
}




// Proxy external requests to bypass CORS restrictions
async fn proxy_external_request(req: web::Json<ProxyRequest>) -> Result<HttpResponse> {
    println!("Proxy request to: {}", req.url);
    
    // Create HTTP client
    let client = reqwest::Client::new();
    
    // Build request
    let mut request_builder = match req.method.as_deref().unwrap_or("GET") {
        "POST" => client.post(&req.url),
        "PUT" => client.put(&req.url),
        "DELETE" => client.delete(&req.url),
        "PATCH" => client.patch(&req.url),
        _ => client.get(&req.url),
    };
    
    // Add headers if provided
    if let Some(headers) = &req.headers {
        for (key, value) in headers {
            request_builder = request_builder.header(key, value);
        }
    }
    
    // Set a reasonable timeout
    request_builder = request_builder.timeout(std::time::Duration::from_secs(30));
    
    match request_builder.send().await {
        Ok(response) => {
            // Get content type to determine how to parse the response
            let content_type = response.headers()
                .get("content-type")
                .and_then(|ct| ct.to_str().ok())
                .unwrap_or("")
                .to_lowercase();
            
            // Try to get the response text first
            match response.text().await {
                Ok(text_data) => {
                    println!("Proxy request successful, returning {} bytes", text_data.len());
                    
                    // Check if it's XML/RSS content
                    if content_type.contains("xml") || content_type.contains("rss") || 
                       text_data.trim_start().starts_with("<?xml") || 
                       text_data.contains("<rss") || text_data.contains("<feed") {
                        // Return as raw text for XML/RSS content
                        Ok(HttpResponse::Ok().json(ProxyResponse {
                            success: true,
                            data: Some(serde_json::Value::String(text_data)),
                            error: None,
                        }))
                    } else {
                        // Try to parse as JSON for non-XML content
                        match serde_json::from_str::<serde_json::Value>(&text_data) {
                            Ok(json_data) => {
                                Ok(HttpResponse::Ok().json(ProxyResponse {
                                    success: true,
                                    data: Some(json_data),
                                    error: None,
                                }))
                            }
                            Err(_) => {
                                // If JSON parsing fails, return as raw text
                                Ok(HttpResponse::Ok().json(ProxyResponse {
                                    success: true,
                                    data: Some(serde_json::Value::String(text_data)),
                                    error: None,
                                }))
                            }
                        }
                    }
                }
                Err(parse_error) => {
                    eprintln!("Failed to parse response as text: {parse_error}");
                    Ok(HttpResponse::InternalServerError().json(ProxyResponse {
                        success: false,
                        data: None,
                        error: Some(format!("Failed to parse response: {parse_error}")),
                    }))
                }
            }
        }
        Err(request_error) => {
            eprintln!("Proxy request failed: {request_error}");
            Ok(HttpResponse::InternalServerError().json(ProxyResponse {
                success: false,
                data: None,
                error: Some(format!("Request failed: {request_error}")),
            }))
        }
    }
}

// Get list of tables with row counts - returns real database tables with accurate counts
async fn get_tables(data: web::Data<Arc<ApiState>>, query: web::Query<std::collections::HashMap<String, String>>) -> Result<HttpResponse> {
    // Check if a specific connection is requested
    let pool = if let Some(connection_name) = query.get("connection") {
        // Get the database URL for this connection
        let database_url = if let Ok(url) = std::env::var(connection_name) {
            // Direct URL environment variable
            url
        } else {
            // Try component-based configuration
            let host_key = format!("{connection_name}_HOST");
            let port_key = format!("{connection_name}_PORT");
            let name_key = format!("{connection_name}_NAME");
            let user_key = format!("{connection_name}_USER");
            let password_key = format!("{connection_name}_PASSWORD");
            let ssl_key = format!("{connection_name}_SSL_MODE");
            
            if let (Ok(host), Ok(port), Ok(name), Ok(user), Ok(password)) = (
                std::env::var(&host_key),
                std::env::var(&port_key),
                std::env::var(&name_key),
                std::env::var(&user_key),
                std::env::var(&password_key)
            ) {
                let ssl_mode = std::env::var(&ssl_key).unwrap_or_else(|_| "require".to_string());
                format!("postgres://{user}:{password}@{host}:{port}/{name}?sslmode={ssl_mode}")
            } else {
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": format!("Connection '{}' not found in environment variables", connection_name)
                })));
            }
        };
        
        // Use the specified connection
        match sqlx::postgres::PgPool::connect(&database_url).await {
            Ok(pool) => pool,
            Err(e) => {
                return Ok(HttpResponse::InternalServerError().json(json!({
                    "error": format!("Failed to connect to {}: {}", connection_name, e)
                })));
            }
        }
    } else {
        // Use default connection
        data.db.clone()
    };
    
    match get_database_tables(&pool, None).await {
        Ok(tables) => {
            let mut table_info = Vec::new();
            
            // Get actual row counts for each table
            for table in tables {
                let query = format!("SELECT COUNT(*) FROM {}", table.name);
                match sqlx::query(&query).fetch_one(&pool).await {
                    Ok(row) => {
                        let count: i64 = row.get(0);
                        table_info.push(TableInfo {
                            name: table.name.clone(),
                            row_count: count,
                        });
                    }
                    Err(_) => {
                        // Table might not be accessible, use estimated count
                        table_info.push(TableInfo {
                            name: table.name.clone(),
                            row_count: table.rows.unwrap_or(0),
                        });
                    }
                }
            }
            
            Ok(HttpResponse::Ok().json(json!({ "tables": table_info })))
        }
        Err(e) => {
            Ok(HttpResponse::InternalServerError().json(json!({
                "error": format!("Failed to fetch tables: {}", e)
            })))
        }
    }
}

// Get list of mock tables - returns hardcoded placeholder data
async fn get_tables_mock() -> Result<HttpResponse> {
    let tables = vec![
        "users", "accounts", "contacts", "opportunities", "activities",
        "campaigns", "documents", "events", "roles", "projects",
        "products", "prospects", "calls", "leads", "surveyquestionoptions",
        "tags", "taggables"
    ];
    
    let table_info: Vec<TableInfo> = tables.iter().map(|table_name| {
        TableInfo {
            name: table_name.to_string(),
            row_count: 0, // Mock data shows 0 rows
        }
    }).collect();
    
    Ok(HttpResponse::Ok().json(json!({ "tables": table_info })))
}

// Test database connection
async fn db_test_connection(data: web::Data<Arc<ApiState>>) -> Result<HttpResponse> {
    match test_db_connection(&data.db).await {
        Ok(info) => Ok(HttpResponse::Ok().json(DatabaseResponse {
            success: true,
            message: Some("Database connection successful".to_string()),
            error: None,
            data: Some(serde_json::to_value(info).unwrap()),
        })),
        Err(e) => Ok(HttpResponse::InternalServerError().json(DatabaseResponse {
            success: false,
            message: None,
            error: Some(format!("Connection failed: {e}")),
            data: None,
        })),
    }
}

// List database tables with detailed info
async fn db_list_tables(
    data: web::Data<Arc<ApiState>>,
    query: web::Query<std::collections::HashMap<String, String>>,
) -> Result<HttpResponse> {
    let limit = query.get("limit").and_then(|s| s.parse::<i32>().ok());
    match get_database_tables(&data.db, limit).await {
        Ok(tables) => Ok(HttpResponse::Ok().json(DatabaseResponse {
            success: true,
            message: Some(format!("Found {} tables", tables.len())),
            error: None,
            data: Some(serde_json::json!({ "tables": tables })),
        })),
        Err(e) => Ok(HttpResponse::InternalServerError().json(DatabaseResponse {
            success: false,
            message: None,
            error: Some(format!("Failed to list tables: {e}")),
            data: None,
        })),
    }
}

// Get table information
async fn db_get_table_info(
    data: web::Data<Arc<ApiState>>,
    path: web::Path<String>,
    query: web::Query<std::collections::HashMap<String, String>>,
) -> Result<HttpResponse> {
    let table_name = path.into_inner();
    
    // Check if a specific connection is requested
    let pool = if let Some(connection_name) = query.get("connection") {
        // Get the database URL for this connection
        let database_url = if let Ok(url) = std::env::var(connection_name) {
            // Direct URL environment variable
            url
        } else {
            // Try component-based configuration
            let host_key = format!("{connection_name}_HOST");
            let port_key = format!("{connection_name}_PORT");
            let name_key = format!("{connection_name}_NAME");
            let user_key = format!("{connection_name}_USER");
            let password_key = format!("{connection_name}_PASSWORD");
            let ssl_key = format!("{connection_name}_SSL_MODE");
            
            if let (Ok(host), Ok(port), Ok(name), Ok(user), Ok(password)) = (
                std::env::var(&host_key),
                std::env::var(&port_key),
                std::env::var(&name_key),
                std::env::var(&user_key),
                std::env::var(&password_key)
            ) {
                let ssl_mode = std::env::var(&ssl_key).unwrap_or_else(|_| "require".to_string());
                format!("postgres://{user}:{password}@{host}:{port}/{name}?sslmode={ssl_mode}")
            } else {
                return Ok(HttpResponse::BadRequest().json(DatabaseResponse {
                    success: false,
                    message: None,
                    error: Some(format!("Connection '{connection_name}' not found in environment variables")),
                    data: None,
                }));
            }
        };
        
        // Use the specified connection
        match sqlx::postgres::PgPool::connect(&database_url).await {
            Ok(pool) => pool,
            Err(e) => {
                return Ok(HttpResponse::InternalServerError().json(DatabaseResponse {
                    success: false,
                    message: None,
                    error: Some(format!("Failed to connect to {connection_name}: {e}")),
                    data: None,
                }));
            }
        }
    } else {
        // Use default connection
        data.db.clone()
    };
    
    match get_table_details(&pool, &table_name).await {
        Ok(info) => Ok(HttpResponse::Ok().json(DatabaseResponse {
            success: true,
            message: Some(format!("Table {table_name} found")),
            error: None,
            data: Some(serde_json::to_value(info).unwrap()),
        })),
        Err(e) => Ok(HttpResponse::InternalServerError().json(DatabaseResponse {
            success: false,
            message: None,
            error: Some(format!("Failed to get table info: {e}")),
            data: None,
        })),
    }
}

// Execute custom query (use with caution!)
async fn db_execute_query(
    data: web::Data<Arc<ApiState>>,
    query_req: web::Json<QueryRequest>,
    query: web::Query<std::collections::HashMap<String, String>>,
) -> Result<HttpResponse> {
    // Only allow safe SELECT queries for security
    let query_text = query_req.query.trim().to_lowercase();
    if !query_text.starts_with("select") {
        return Ok(HttpResponse::BadRequest().json(DatabaseResponse {
            success: false,
            message: None,
            error: Some("Only SELECT queries are allowed".to_string()),
            data: None,
        }));
    }

    // Check if a specific connection is requested
    let pool = if let Some(connection_name) = query.get("connection") {
        // Get the database URL for this connection
        let database_url = if let Ok(url) = std::env::var(connection_name) {
            // Direct URL environment variable
            url
        } else {
            // Try component-based configuration
            let host_key = format!("{connection_name}_HOST");
            let port_key = format!("{connection_name}_PORT");
            let name_key = format!("{connection_name}_NAME");
            let user_key = format!("{connection_name}_USER");
            let password_key = format!("{connection_name}_PASSWORD");
            let ssl_key = format!("{connection_name}_SSL_MODE");
            
            if let (Ok(host), Ok(port), Ok(name), Ok(user), Ok(password)) = (
                std::env::var(&host_key),
                std::env::var(&port_key),
                std::env::var(&name_key),
                std::env::var(&user_key),
                std::env::var(&password_key)
            ) {
                let ssl_mode = std::env::var(&ssl_key).unwrap_or_else(|_| "require".to_string());
                format!("postgres://{user}:{password}@{host}:{port}/{name}?sslmode={ssl_mode}")
            } else {
                return Ok(HttpResponse::BadRequest().json(DatabaseResponse {
                    success: false,
                    message: None,
                    error: Some(format!("Connection '{connection_name}' not found in environment variables")),
                    data: None,
                }));
            }
        };
        
        // Use the specified connection
        match sqlx::postgres::PgPool::connect(&database_url).await {
            Ok(pool) => pool,
            Err(e) => {
                return Ok(HttpResponse::InternalServerError().json(DatabaseResponse {
                    success: false,
                    message: None,
                    error: Some(format!("Failed to connect to {connection_name}: {e}")),
                    data: None,
                }));
            }
        }
    } else {
        // Use default connection
        data.db.clone()
    };

    match execute_safe_query(&pool, &query_req.query).await {
        Ok(result) => Ok(HttpResponse::Ok().json(DatabaseResponse {
            success: true,
            message: Some("Query executed successfully".to_string()),
            error: None,
            data: Some(result),
        })),
        Err(e) => Ok(HttpResponse::InternalServerError().json(DatabaseResponse {
            success: false,
            message: None,
            error: Some(format!("Query failed: {e}")),
            data: None,
        })),
    }
}

// Create a new project
// Get all projects from database
async fn get_projects(data: web::Data<Arc<ApiState>>) -> Result<HttpResponse> {
    let projects_query = sqlx::query(
        "SELECT id, name, description, status, date_entered, date_modified FROM projects ORDER BY date_modified DESC LIMIT 50"
    )
    .fetch_all(&data.db)
    .await;
    
    match projects_query {
        Ok(rows) => {
            let projects: Vec<serde_json::Value> = rows.into_iter().map(|row| {
                json!({
                    "id": row.get::<Uuid, _>("id"),
                    "name": row.get::<String, _>("name"),
                    "description": row.get::<Option<String>, _>("description"),
                    "status": row.get::<Option<String>, _>("status"),
                    "created_date": row.get::<chrono::DateTime<Utc>, _>("date_entered"),
                    "modified_date": row.get::<chrono::DateTime<Utc>, _>("date_modified")
                })
            }).collect();
            
            Ok(HttpResponse::Ok().json(json!({
                "success": true,
                "data": projects
            })))
        },
        Err(e) => {
            println!("Error fetching projects: {e}");
            // Return empty array if database query fails
            Ok(HttpResponse::Ok().json(json!({
                "success": true,
                "data": []
            })))
        }
    }
}

async fn create_project(
    data: web::Data<Arc<ApiState>>,
    req: web::Json<CreateProjectRequest>,
) -> Result<HttpResponse> {
    let id = Uuid::new_v4();
    let now = Utc::now();
    
    // Parse date strings into NaiveDate
    let start_date = req.estimated_start_date.as_ref()
        .and_then(|s| if s.is_empty() { None } else { Some(s) })
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
    
    let end_date = req.estimated_end_date.as_ref()
        .and_then(|s| if s.is_empty() { None } else { Some(s) })
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
    
    let result = sqlx::query(
        r#"
        INSERT INTO projects (
            id, name, description, status, 
            estimated_start_date, estimated_end_date,
            date_entered, date_modified, created_by, modified_user_id
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        "#
    )
    .bind(id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.status)
    .bind(start_date)
    .bind(end_date)
    .bind(now)
    .bind(now)
    .bind("1") // Default user ID
    .bind("1") // Default user ID
    .execute(&data.db)
    .await;
    
    match result {
        Ok(_) => Ok(HttpResponse::Created().json(json!({
            "id": id.to_string(),
            "message": "Project created successfully"
        }))),
        Err(e) => Ok(HttpResponse::BadRequest().json(json!({
            "error": e.to_string()
        }))),
    }
}

// Initialize database schema (simplified version with core tables)
async fn init_database(pool: &Pool<Postgres>) -> anyhow::Result<()> {
    // Create users table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            user_name VARCHAR(60),
            first_name VARCHAR(30),
            last_name VARCHAR(30),
            email VARCHAR(100),
            status VARCHAR(100),
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
        )
        "#
    ).execute(pool).await?;
    
    // Create accounts table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS accounts (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name VARCHAR(150),
            account_type VARCHAR(50),
            industry VARCHAR(50),
            phone_office VARCHAR(100),
            website VARCHAR(255),
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create contacts table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS contacts (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            salutation VARCHAR(255),
            first_name VARCHAR(100),
            last_name VARCHAR(100),
            title VARCHAR(100),
            department VARCHAR(255),
            account_id UUID REFERENCES accounts(id),
            phone_work VARCHAR(100),
            phone_mobile VARCHAR(100),
            email VARCHAR(100),
            primary_address_street VARCHAR(150),
            primary_address_city VARCHAR(100),
            primary_address_state VARCHAR(100),
            primary_address_postalcode VARCHAR(20),
            primary_address_country VARCHAR(255),
            description TEXT,
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create projects table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS projects (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name VARCHAR(50),
            description TEXT,
            status VARCHAR(50),
            priority VARCHAR(255),
            estimated_start_date DATE,
            estimated_end_date DATE,
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create opportunities table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS opportunities (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name VARCHAR(50),
            account_id UUID REFERENCES accounts(id),
            opportunity_type VARCHAR(255),
            lead_source VARCHAR(50),
            amount DECIMAL(26,6),
            currency_id VARCHAR(36),
            date_closed DATE,
            sales_stage VARCHAR(255),
            probability DECIMAL(3,0),
            description TEXT,
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create activities table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS activities (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name VARCHAR(255),
            date_due TIMESTAMP WITH TIME ZONE,
            date_start TIMESTAMP WITH TIME ZONE,
            parent_type VARCHAR(255),
            parent_id UUID,
            status VARCHAR(100),
            priority VARCHAR(255),
            description TEXT,
            contact_id UUID REFERENCES contacts(id),
            account_id UUID REFERENCES accounts(id),
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create leads table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS leads (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            salutation VARCHAR(255),
            first_name VARCHAR(100),
            last_name VARCHAR(100),
            title VARCHAR(100),
            company VARCHAR(100),
            phone_work VARCHAR(100),
            phone_mobile VARCHAR(100),
            email VARCHAR(100),
            status VARCHAR(100),
            lead_source VARCHAR(100),
            description TEXT,
            converted BOOLEAN DEFAULT false,
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create campaigns table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS campaigns (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name VARCHAR(50),
            campaign_type VARCHAR(100),
            status VARCHAR(100),
            start_date DATE,
            end_date DATE,
            budget DECIMAL(26,6),
            expected_cost DECIMAL(26,6),
            actual_cost DECIMAL(26,6),
            expected_revenue DECIMAL(26,6),
            objective TEXT,
            content TEXT,
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create documents table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS documents (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            document_name VARCHAR(255),
            filename VARCHAR(255),
            file_ext VARCHAR(100),
            file_mime_type VARCHAR(100),
            revision VARCHAR(100),
            category_id VARCHAR(100),
            subcategory_id VARCHAR(100),
            status VARCHAR(100),
            description TEXT,
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create events table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS events (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name VARCHAR(255),
            date_start TIMESTAMP WITH TIME ZONE,
            date_end TIMESTAMP WITH TIME ZONE,
            duration_hours INTEGER,
            duration_minutes INTEGER,
            location VARCHAR(255),
            description TEXT,
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create products table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS products (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name VARCHAR(50),
            product_code VARCHAR(50),
            category VARCHAR(100),
            manufacturer VARCHAR(50),
            cost DECIMAL(26,6),
            price DECIMAL(26,6),
            description TEXT,
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create roles table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS roles (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name VARCHAR(150),
            description TEXT,
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create calls table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS calls (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name VARCHAR(50),
            date_start TIMESTAMP WITH TIME ZONE,
            date_end TIMESTAMP WITH TIME ZONE,
            duration_hours INTEGER,
            duration_minutes INTEGER,
            status VARCHAR(100),
            direction VARCHAR(100),
            parent_type VARCHAR(255),
            parent_id UUID,
            contact_id UUID REFERENCES contacts(id),
            account_id UUID REFERENCES accounts(id),
            description TEXT,
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create surveyquestionoptions table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS surveyquestionoptions (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name VARCHAR(50),
            survey_question_id UUID,
            sort_order INTEGER,
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            created_by VARCHAR(36),
            modified_user_id VARCHAR(36)
        )
        "#
    ).execute(pool).await?;
    
    // Create tags table
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS tags (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name VARCHAR(255),
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            date_modified TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
        )
        "#
    ).execute(pool).await?;
    
    // Create taggables table (polymorphic relationship)
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS taggables (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            tag_id UUID REFERENCES tags(id),
            taggable_type VARCHAR(100),
            taggable_id UUID,
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(tag_id, taggable_type, taggable_id)
        )
        "#
    ).execute(pool).await?;
    
    // Create relationship tables
    
    // User roles relationship
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users_roles (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            user_id UUID REFERENCES users(id),
            role_id UUID REFERENCES roles(id),
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(user_id, role_id)
        )
        "#
    ).execute(pool).await?;
    
    // Account contacts relationship
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS accounts_contacts (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            account_id UUID REFERENCES accounts(id),
            contact_id UUID REFERENCES contacts(id),
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(account_id, contact_id)
        )
        "#
    ).execute(pool).await?;
    
    // Account opportunities relationship
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS accounts_opportunities (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            account_id UUID REFERENCES accounts(id),
            opportunity_id UUID REFERENCES opportunities(id),
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(account_id, opportunity_id)
        )
        "#
    ).execute(pool).await?;
    
    // Contact opportunities relationship
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS contacts_opportunities (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            contact_id UUID REFERENCES contacts(id),
            opportunity_id UUID REFERENCES opportunities(id),
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(contact_id, opportunity_id)
        )
        "#
    ).execute(pool).await?;
    
    // Campaign leads relationship
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS campaigns_leads (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            campaign_id UUID REFERENCES campaigns(id),
            lead_id UUID REFERENCES leads(id),
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(campaign_id, lead_id)
        )
        "#
    ).execute(pool).await?;
    
    // Project contacts relationship
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS projects_contacts (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            project_id UUID REFERENCES projects(id),
            contact_id UUID REFERENCES contacts(id),
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(project_id, contact_id)
        )
        "#
    ).execute(pool).await?;
    
    // Project accounts relationship
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS projects_accounts (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            project_id UUID REFERENCES projects(id),
            account_id UUID REFERENCES accounts(id),
            date_entered TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
            UNIQUE(project_id, account_id)
        )
        "#
    ).execute(pool).await?;
    
    println!("Database schema initialized successfully!");
    Ok(())
}

// Helper functions for database admin endpoints
async fn test_db_connection(pool: &Pool<Postgres>) -> Result<ConnectionInfo, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT 
            version() as server_version,
            current_database() as database_name,
            current_user as current_user,
            (SELECT count(*) FROM pg_stat_activity) as connection_count
        "#,
    )
    .fetch_one(pool)
    .await?;

    Ok(ConnectionInfo {
        server_version: row.get("server_version"),
        database_name: row.get("database_name"),
        current_user: row.get("current_user"),
        connection_count: row.get("connection_count"),
    })
}

async fn get_database_tables(pool: &Pool<Postgres>, limit: Option<i32>) -> Result<Vec<TableInfoDetailed>, sqlx::Error> {
    let query = if let Some(limit_val) = limit {
        format!(
            r#"
            SELECT 
                table_name,
                (
                    SELECT reltuples::bigint 
                    FROM pg_class 
                    WHERE relname = table_name
                ) as estimated_rows
            FROM information_schema.tables 
            WHERE table_schema = 'public' 
                AND table_type = 'BASE TABLE'
            ORDER BY table_name
            LIMIT {limit_val}
            "#
        )
    } else {
        r#"
        SELECT 
            table_name,
            (
                SELECT reltuples::bigint 
                FROM pg_class 
                WHERE relname = table_name
            ) as estimated_rows
        FROM information_schema.tables 
        WHERE table_schema = 'public' 
            AND table_type = 'BASE TABLE'
        ORDER BY table_name
        "#.to_string()
    };
    
    let rows = sqlx::query(&query)
    .fetch_all(pool)
    .await?;

    let mut tables = Vec::new();
    for row in rows {
        let table_name: String = row.get("table_name");
        let estimated_rows: Option<i64> = row.get("estimated_rows");
        
        // Add description based on table name
        let description = get_table_description(&table_name);
        
        tables.push(TableInfoDetailed {
            name: table_name,
            rows: estimated_rows,
            description,
        });
    }

    Ok(tables)
}

async fn get_table_details(pool: &Pool<Postgres>, table_name: &str) -> Result<HashMap<String, serde_json::Value>, sqlx::Error> {
    // Get basic table info
    let row = sqlx::query(
        r#"
        SELECT 
            (SELECT reltuples::bigint FROM pg_class WHERE relname = $1) as estimated_rows,
            (SELECT count(*) FROM information_schema.columns WHERE table_name = $1) as column_count
        "#,
    )
    .bind(table_name)
    .fetch_one(pool)
    .await?;

    // Get column information
    let column_rows = sqlx::query(
        r#"
        SELECT 
            column_name,
            data_type,
            is_nullable,
            column_default,
            character_maximum_length,
            numeric_precision,
            numeric_scale
        FROM information_schema.columns 
        WHERE table_name = $1 
        ORDER BY ordinal_position
        "#,
    )
    .bind(table_name)
    .fetch_all(pool)
    .await?;

    let mut columns = Vec::new();
    for col_row in column_rows {
        let mut column_info = serde_json::Map::new();
        column_info.insert("name".to_string(), serde_json::Value::String(col_row.get::<String, _>("column_name")));
        column_info.insert("type".to_string(), serde_json::Value::String(col_row.get::<String, _>("data_type")));
        column_info.insert("nullable".to_string(), serde_json::Value::String(col_row.get::<String, _>("is_nullable")));
        
        if let Some(default_value) = col_row.get::<Option<String>, _>("column_default") {
            column_info.insert("default".to_string(), serde_json::Value::String(default_value));
        }
        
        if let Some(max_length) = col_row.get::<Option<i32>, _>("character_maximum_length") {
            column_info.insert("max_length".to_string(), serde_json::json!(max_length));
        }
        
        columns.push(serde_json::Value::Object(column_info));
    }

    let mut info = HashMap::new();
    info.insert("table_name".to_string(), serde_json::Value::String(table_name.to_string()));
    info.insert("estimated_rows".to_string(), serde_json::json!(row.get::<Option<i64>, _>("estimated_rows")));
    info.insert("column_count".to_string(), serde_json::json!(row.get::<i64, _>("column_count")));
    info.insert("description".to_string(), serde_json::Value::String(
        get_table_description(table_name).unwrap_or_else(|| "No description available".to_string())
    ));
    info.insert("columns".to_string(), serde_json::Value::Array(columns));

    Ok(info)
}

async fn execute_safe_query(pool: &Pool<Postgres>, query: &str) -> Result<serde_json::Value, sqlx::Error> {
    let rows = sqlx::query(query).fetch_all(pool).await?;
    
    let mut results = Vec::new();
    for row in rows {
        let mut row_map = serde_json::Map::new();
        
        // This is a simplified approach - in production you'd want to handle types properly
        for (i, column) in row.columns().iter().enumerate() {
            let value = match row.try_get_raw(i) {
                Ok(raw_value) => {
                    // Try to convert to string for simplicity
                    if raw_value.is_null() {
                        serde_json::Value::Null
                    } else {
                        // For demo purposes, try to get as string or show type info
                        match row.try_get::<String, _>(i) {
                            Ok(s) => serde_json::Value::String(s),
                            Err(_) => serde_json::Value::String("Non-string value".to_string()),
                        }
                    }
                }
                Err(_) => serde_json::Value::String("Error reading value".to_string()),
            };
            
            row_map.insert(column.name().to_string(), value);
        }
        
        results.push(serde_json::Value::Object(row_map));
    }

    Ok(serde_json::Value::Array(results))
}

fn get_table_description(table_name: &str) -> Option<String> {
    match table_name {
        "accounts" => Some("Customer accounts and organizations".to_string()),
        "contacts" => Some("Individual contact records".to_string()),
        "users" => Some("System users and administrators".to_string()),
        "opportunities" => Some("Sales opportunities and deals".to_string()),
        "cases" => Some("Customer support cases".to_string()),
        "leads" => Some("Sales leads and prospects".to_string()),
        "campaigns" => Some("Marketing campaigns".to_string()),
        "meetings" => Some("Scheduled meetings and appointments".to_string()),
        "calls" => Some("Phone calls and communications".to_string()),
        "tasks" => Some("Tasks and activities".to_string()),
        "projects" => Some("Project management records".to_string()),
        "project_task" => Some("Individual project tasks".to_string()),
        "documents" => Some("Document attachments and files".to_string()),
        "emails" => Some("Email communications".to_string()),
        "notes" => Some("Notes and comments".to_string()),
        "activities" => Some("Activities and tasks".to_string()),
        "surveyquestionoptions" => Some("Survey question options".to_string()),
        "tags" => Some("Tags for categorization".to_string()),
        "taggables" => Some("Polymorphic tag relationships".to_string()),
        "roles" => Some("User roles and permissions".to_string()),
        _ => None,
    }
}

// Run the API server
async fn run_api_server(config: Config) -> anyhow::Result<()> {
    println!("Attempting to connect to database: {}", &config.database_url);
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database_url)
        .await
        .context("Failed to connect to database")?;
    
    println!("Database connection successful!");
    
    // Create shared config for hot reloading
    let shared_config = Arc::new(Mutex::new(config));
    
    // Start watching .env file for changes
    if let Err(e) = start_env_watcher(shared_config.clone()) {
        log::warn!("Failed to start .env file watcher: {e}");
    }
    
    let state = Arc::new(ApiState {
        db: pool,
        config: shared_config.clone(),
    });
    
    // Create persistent Claude session manager
    let claude_session_manager: ClaudeSessionManager = Arc::new(Mutex::new(ClaudeSession::new()));
    
    // Create security components
    let rate_limiter = Arc::new(RateLimiter::new());
    let csrf_store: CsrfTokenStore = Arc::new(Mutex::new(HashMap::new()));
    
    // Get server config from shared config
    let (server_host, server_port, _is_production) = {
        let config_guard = shared_config.lock().unwrap();
        (config_guard.server_host.clone(), config_guard.server_port, config_guard.is_production)
    };
    
    println!("Starting API server on {server_host}:{server_port}");
    let session_manager_clone = claude_session_manager.clone();
    let rate_limiter_clone = rate_limiter.clone();
    let csrf_store_clone = csrf_store.clone();
    
    HttpServer::new(move || {
        let cors = Cors::default()
            .allowed_origin("http://localhost:8887") // Specific frontend origin
            .allowed_origin("http://localhost:8888") // Alternative frontend port
            .allow_any_method()
            .allow_any_header()
            .supports_credentials() // Enable credentials for session cookies
            .max_age(3600);
        
        let session_key = {
            let config_guard = state.config.lock().unwrap();
            actix_web::cookie::Key::from(config_guard.session_key.as_bytes())
        };
        
        App::new()
            .app_data(web::Data::new(state.clone()))
            .app_data(web::Data::new(shared_config.clone()))
            .app_data(web::Data::new(session_manager_clone.clone()))
            .app_data(web::Data::new(rate_limiter_clone.clone()))
            .app_data(web::Data::new(csrf_store_clone.clone()))
            .wrap(cors)
            .wrap(middleware::Logger::default())
            .wrap({
                let config_guard = shared_config.lock().unwrap();
                let is_production = config_guard.is_production;
                drop(config_guard);
                
                SessionMiddleware::builder(CookieSessionStore::default(), session_key)
                    .cookie_secure(is_production) // HTTPS required in production
                    .cookie_domain(if is_production { None } else { Some("localhost".to_string()) })
                    .cookie_same_site(if is_production { SameSite::Strict } else { SameSite::Lax })
                    .cookie_path("/".to_string())
                    .cookie_name("membercommons_session".to_string())
                    .cookie_http_only(true) // Always prevent JavaScript access
                    .session_lifecycle(
                        actix_session::config::PersistentSession::default()
                            .session_ttl(CookieDuration::seconds(24 * 60 * 60))
                    )
                    .build()
            })
            .service(
                web::scope("/api")
                    .route("/health", web::get().to(health_check))
                    .route("/tables", web::get().to(get_tables))
                    .route("/tables/mock", web::get().to(get_tables_mock))
                    .route("/projects", web::get().to(get_projects))
                    .route("/projects", web::post().to(create_project))
                    .service(
                        web::scope("/db")
                            .route("/test-connection", web::get().to(db_test_connection))
                            .route("/tables", web::get().to(db_list_tables))
                            .route("/table/{table_name}", web::get().to(db_get_table_info))
                            .route("/query", web::post().to(db_execute_query))
                    )
                    .service(
                        web::scope("/import")
                            .route("/excel", web::post().to(import::import_excel_data))
                            .route("/excel/preview", web::post().to(import::preview_excel_data))
                            .route("/excel/sheets", web::post().to(import::get_excel_sheets))
                            .route("/data", web::post().to(import::import_data))
                            .route("/democracylab", web::post().to(import::import_democracylab_projects))
                    )
                    .service(
                        web::scope("/claude")
                            .route("/usage/cli", web::get().to(get_claude_usage_cli))
                            .route("/usage/website", web::get().to(get_claude_usage_website))
                            .route("/analyze", web::post().to(claude_insights::analyze_with_claude_cli))
                    )
                    .service(
                        web::scope("/gemini")
                            .route("/usage/cli", web::get().to(get_gemini_usage_cli))
                            .route("/usage/website", web::get().to(get_gemini_usage_website))
                            .route("/analyze", web::post().to(gemini_insights::analyze_with_gemini))
                    )
                    .service(
                        web::scope("/auth")
                            // Google OAuth
                            .service(
                                web::scope("/google")
                                    .route("/verify", web::post().to(verify_google_auth))
                                    .route("/url", web::get().to(google_auth_url))
                                    .route("/callback", web::get().to(google_auth_callback))
                            )
                            // LinkedIn OAuth
                            .service(
                                web::scope("/linkedin")
                                    .route("/url", web::get().to(linkedin_auth_url))
                                    .route("/callback", web::get().to(linkedin_auth_callback))
                            )
                            // GitHub OAuth  
                            .service(
                                web::scope("/github")
                                    .route("/url", web::get().to(github_auth_url))
                                    .route("/callback", web::get().to(github_auth_callback))
                            )
                            // Common auth endpoints
                            .route("/user", web::get().to(get_current_user))
                            .route("/logout", web::post().to(logout_user))
                            .route("/debug", web::get().to(debug_oauth_config))
                            .route("/session-debug", web::get().to(debug_session))
                            // Supabase integration
                            .route("/supabase/verify", web::post().to(verify_supabase_token))
                            .route("/supabase/session", web::post().to(create_supabase_session))
                    )
                    .service(
                        web::scope("/google")
                            .route("/create-project", web::post().to(create_google_project))
                            .service(
                                web::scope("/sheets")
                                    .route("/config", web::get().to(get_sheets_config))
                                    .route("/config", web::post().to(save_sheets_config))
                                    .route("/member/{email}", web::get().to(get_member_by_email))
                                    .route("/member", web::post().to(save_member_data))
                                    .route("/member", web::put().to(save_member_data))
                            )
                            .service(
                                web::scope("/gemini")
                                    .route("/analyze", web::post().to(gemini_insights::analyze_with_gemini))
                            )
                    )
                    .service(
                        web::scope("/config")
                            .route("/current", web::get().to(get_current_config))
                            .route("/env", web::get().to(get_env_config))
                            .route("/env", web::post().to(save_env_config))
                            .route("/env/create", web::post().to(create_env_config))
                            .route("/gemini", web::get().to(gemini_insights::test_gemini_api))
                            .route("/restart", web::post().to(restart_server))
                    )
                    .service(
                        web::scope("/proxy")
                            .route("/csv", web::post().to(fetch_csv))
                            .route("/external", web::post().to(proxy_external_request))
                    )
                    .service(
                        web::scope("/recommendations")
                            .route("", web::post().to(get_recommendations_handler))
                    )
            )
    })
    .bind((server_host, server_port))?
    .run()
    .await?;

    Ok(())
}

// Function to get persistent Claude CLI usage data
async fn get_claude_cli_usage_persistent(session_manager: ClaudeSessionManager) -> anyhow::Result<serde_json::Value> {
    let mut session = session_manager.lock().unwrap();
    
    // Check if we need to start a new session
    if !session.is_active() {
        println!("Starting new persistent Claude CLI session...");
        session.prompt_count = 0;
        session.total_input_tokens = 0;
        session.total_output_tokens = 0;
    }
    
    // Increment prompt count for this session
    session.prompt_count += 1;
    let current_prompt_count = session.prompt_count;
    
    // Send a small prompt to get current usage data
    let prompt = format!("This is prompt #{current_prompt_count} in our persistent session. What is 2+2?");
    
    println!("Sending prompt #{current_prompt_count} to Claude CLI persistent session...");
    
    // Execute Claude CLI command with JSON output
    let output = Command::new("claude")
        .arg("--print")
        .arg("--output-format")
        .arg("json")
        .arg(&prompt)
        .output()
        .context("Failed to execute claude command. Make sure Claude CLI is installed and accessible.")?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("Claude CLI command failed: {stderr}"));
    }
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout_str = stdout.trim();
    
    if stdout_str.is_empty() {
        return Err(anyhow::anyhow!("Claude CLI returned empty response"));
    }
    
    // Parse the JSON response
    if let Ok(json_data) = serde_json::from_str::<serde_json::Value>(stdout_str) {
        // Extract usage information if available
        if let Some(usage) = json_data.get("usage") {
            println!("Found usage data in Claude CLI response: {usage:?}");
            
            // Update session tracking with new usage data
            if let Some(input_tokens) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                session.total_input_tokens = input_tokens as u32;
            }
            if let Some(output_tokens) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
                session.total_output_tokens += output_tokens as u32; // Accumulate output tokens
            }
            
            // Store the latest usage data
            session.last_usage = Some(usage.clone());
            
            // Create enhanced usage data with session info
            let enhanced_usage = json!({
                "input_tokens": usage.get("input_tokens").unwrap_or(&json!(0)),
                "output_tokens": usage.get("output_tokens").unwrap_or(&json!(0)),
                "cache_creation_input_tokens": usage.get("cache_creation_input_tokens").unwrap_or(&json!(0)),
                "cache_read_input_tokens": usage.get("cache_read_input_tokens").unwrap_or(&json!(0)),
                "service_tier": usage.get("service_tier").unwrap_or(&json!("standard")),
                "session_info": {
                    "prompt_count": current_prompt_count,
                    "session_duration_seconds": session.get_session_duration(),
                    "total_accumulated_output_tokens": session.total_output_tokens,
                    "session_start_timestamp": session.session_start
                }
            });
            
            return Ok(enhanced_usage);
        }
        
        // If no usage field, create session status
        let usage_data = json!({
            "connection_status": "connected",
            "session_info": {
                "prompt_count": current_prompt_count,
                "session_duration_seconds": session.get_session_duration(),
                "total_accumulated_output_tokens": session.total_output_tokens,
                "session_start_timestamp": session.session_start
            },
            "note": "Claude CLI is connected and working, but usage data is not available through the CLI"
        });
        
        println!("Claude CLI persistent session active, returning status: {usage_data:?}");
        return Ok(usage_data);
    }
    
    // If JSON parsing fails, Claude CLI might not be working properly
    Err(anyhow::anyhow!("Claude CLI response could not be parsed as JSON: {stdout_str}"))
}

// Fallback function for non-persistent usage (keeping for compatibility)
async fn get_claude_cli_usage() -> anyhow::Result<serde_json::Value> {
    println!("Using fallback one-time Claude CLI request...");
    
    let output = Command::new("claude")
        .arg("--print")
        .arg("--output-format")
        .arg("json")
        .arg("What is 1+1?")
        .output()
        .context("Failed to execute claude command")?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("Claude CLI command failed: {stderr}"));
    }
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout_str = stdout.trim();
    
    if let Ok(json_data) = serde_json::from_str::<serde_json::Value>(stdout_str) {
        if let Some(usage) = json_data.get("usage") {
            return Ok(usage.clone());
        }
    }
    
    Err(anyhow::anyhow!("Could not extract usage data"))
}


// Handlers for Claude usage - get real data from persistent Claude CLI session
async fn get_claude_usage_cli(session_manager: web::Data<ClaudeSessionManager>) -> Result<HttpResponse> {
    match get_claude_cli_usage_persistent(session_manager.get_ref().clone()).await {
        Ok(usage_data) => Ok(HttpResponse::Ok().json(json!({
            "success": true,
            "usage": usage_data
        }))),
        Err(e) => {
            // Fall back to one-time request if persistent session fails
            println!("Persistent session failed, falling back to one-time request: {e}");
            match get_claude_cli_usage().await {
                Ok(fallback_data) => Ok(HttpResponse::Ok().json(json!({
                    "success": true,
                    "usage": fallback_data
                }))),
                Err(fallback_e) => Ok(HttpResponse::Ok().json(json!({
                    "success": false,
                    "error": format!("Failed to get Claude CLI usage: {fallback_e}")
                })))
            }
        }
    }
}

async fn get_claude_usage_website(session_manager: web::Data<ClaudeSessionManager>) -> Result<HttpResponse> {
    // For website usage, we'll use the same persistent CLI session since that's what's available
    match get_claude_cli_usage_persistent(session_manager.get_ref().clone()).await {
        Ok(usage_data) => Ok(HttpResponse::Ok().json(json!({
            "success": true,
            "usage": usage_data
        }))),
        Err(e) => {
            // Fall back to one-time request if persistent session fails  
            println!("Persistent session failed, falling back to one-time request: {e}");
            match get_claude_cli_usage().await {
                Ok(fallback_data) => Ok(HttpResponse::Ok().json(json!({
                    "success": true,
                    "usage": fallback_data
                }))),
                Err(fallback_e) => Ok(HttpResponse::Ok().json(json!({
                    "success": false,
                    "error": format!("Failed to get Claude usage: {fallback_e}")
                })))
            }
        }
    }
}

async fn get_gemini_usage_cli() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(json!({
        "success": false,
        "error": "Gemini CLI not connected or not available"
    })))
}

async fn get_gemini_usage_website() -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(json!({
        "success": false,
        "error": "Gemini website API not configured"
    })))
}

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));
    let config = Config::from_env()?;
    
    // Check for CLI commands
    let cli = Cli::try_parse();
    match cli {
        Ok(cli) => {
            match cli.command {
                Commands::Serve => {
                    run_api_server(config).await?;
                }
                Commands::InitDb => {
                    println!("Initializing database...");
                    let pool = PgPoolOptions::new()
                        .connect(&config.database_url)
                        .await
                        .context("Failed to connect to database for init")?;
                    init_database(&pool).await?;
                }
            }
        }
        Err(_) => {
            // Default to serve if no command is provided
            run_api_server(config).await?;
        }
    }
    
    Ok(())
}