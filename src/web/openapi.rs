use axum::{
    http::header,
    response::{IntoResponse, Response},
};

const DOCUMENT: &str = r##"{
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
    "/mcp": { "post": { "summary": "MCP tools (2026-07-28; legacy 2025-11-25, 2025-06-18, 2025-03-26 initialize)", "description": "Supports MCP versions [\"2026-07-28\", \"2025-11-25\", \"2025-06-18\", \"2025-03-26\"] statelessly. Modern calls use per-request metadata and server/discover; legacy initialize supports all three 2025 revisions.", "requestBody": { "required": true, "content": { "application/json": { "schema": { "oneOf": [{ "$ref": "#/components/schemas/JsonRpcRequest" }, { "$ref": "#/components/schemas/JsonRpcNotification" }] } } } }, "responses": { "200": { "description": "JSON-RPC 2.0 response", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/JsonRpcResponse" } } } }, "202": { "description": "Accepted notification; empty response" }, "400": { "description": "Malformed request or invalid protocol metadata", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/JsonRpcErrorResponse" } } } }, "403": { "description": "Origin does not match request host" }, "404": { "description": "Unknown JSON-RPC method", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/JsonRpcErrorResponse" } } } }, "405": { "description": "POST is required" }, "413": { "description": "Request body exceeds the HTTP limit" } } } },
    "/.well-known/qed.json": { "get": { "summary": "Public verification key", "responses": { "200": { "description": "Ed25519 key metadata" } } } }
  },
  "components": {
    "schemas": {
      "JsonRpcRequest": {
        "type": "object",
        "required": ["jsonrpc", "id", "method"],
        "properties": {
          "jsonrpc": { "const": "2.0" },
          "id": { "oneOf": [{ "type": "string" }, { "type": "number" }] },
          "method": { "type": "string" },
          "params": { "type": "object" }
        }
      },
      "JsonRpcNotification": {
        "type": "object",
        "required": ["jsonrpc", "method"],
        "properties": {
          "jsonrpc": { "const": "2.0" },
          "method": { "type": "string" },
          "params": { "type": "object" }
        }
      },
      "JsonRpcResponse": {
        "oneOf": [
          { "$ref": "#/components/schemas/JsonRpcResultResponse" },
          { "$ref": "#/components/schemas/JsonRpcErrorResponse" }
        ]
      },
      "JsonRpcResultResponse": {
        "type": "object",
        "required": ["jsonrpc", "id", "result"],
        "properties": {
          "jsonrpc": { "const": "2.0" },
          "id": { "oneOf": [{ "type": "string" }, { "type": "number" }] },
          "result": { "type": "object" }
        }
      },
      "JsonRpcErrorResponse": {
        "type": "object",
        "required": ["jsonrpc", "error"],
        "properties": {
          "jsonrpc": { "const": "2.0" },
          "id": { "oneOf": [{ "type": "string" }, { "type": "number" }, { "type": "null" }] },
          "error": {
            "type": "object",
            "required": ["code", "message"],
            "properties": {
              "code": { "type": "integer" },
              "message": { "type": "string" }
            }
          }
        }
      }
    }
  }
}"##;

pub(crate) async fn document() -> Response {
    ([(header::CONTENT_TYPE, "application/json; charset=utf-8")], DOCUMENT).into_response()
}
