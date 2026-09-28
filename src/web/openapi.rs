use axum::{
    http::header,
    response::{IntoResponse, Response},
};

const DOCUMENT: &str = r#"{
  "openapi": "3.1.0",
  "info": {
    "title": "QED verification API",
    "version": "1.0.0",
    "description": "Read-only pool checks, signed attestations, registry data, and verification metadata."
  },
  "paths": {
    "/healthz": { "get": { "summary": "Health", "responses": { "200": { "description": "Service is healthy" } } } },
    "/api/registry": { "get": { "summary": "Issuer registry", "responses": { "200": { "description": "Registry entries" } } } },
    "/api/pools/featured": { "get": { "summary": "Featured pools", "responses": { "200": { "description": "Curated pool list" } } } },
    "/api/leaderboard": { "get": { "summary": "Leaderboard page", "parameters": [{ "name": "page", "in": "query", "schema": { "type": "integer", "minimum": 1 } }, { "name": "per", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 50 } }, { "name": "sort", "in": "query", "schema": { "type": "string", "enum": ["volume", "price", "change", "liquidity"] } }, { "name": "dir", "in": "query", "schema": { "type": "string", "enum": ["asc", "desc"] } }], "responses": { "200": { "description": "Ranked leaderboard page" } } } },
    "/api/prices": { "get": { "summary": "Pool prices", "parameters": [{ "name": "ids", "in": "query", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "Price snapshot" } } } },
    "/api/status": { "get": { "summary": "Freshness status", "responses": { "200": { "description": "Service status" } } } },
    "/api/check/{address}": { "get": { "summary": "Check a pool or token", "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "Check result" } } } },
    "/api/wallet": { "post": { "summary": "Check stock-token holdings", "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["address"], "properties": { "address": { "type": "string" } } } } } }, "responses": { "200": { "description": "Known stock-token holdings and contract verdicts" }, "404": { "description": "Unsupported or invalid address" } } } },
    "/api/attest/{id}": { "get": { "summary": "Fetch a signed attestation", "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "pattern": "^[0-9a-fA-F]{64}$" } }], "responses": { "200": { "description": "Attestation JSON" }, "404": { "description": "Not found" } } } },
    "/verify": { "post": { "summary": "Verify an attestation", "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } }, "responses": { "200": { "description": "Five verification booleans" } } } },
    "/.well-known/qed.json": { "get": { "summary": "Public verification key", "responses": { "200": { "description": "Ed25519 key metadata" } } } }
  }
}"#;

pub(crate) async fn document() -> Response {
    ([(header::CONTENT_TYPE, "application/json; charset=utf-8")], DOCUMENT).into_response()
}
