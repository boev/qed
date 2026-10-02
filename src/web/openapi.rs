use axum::{
    http::header,
    response::{IntoResponse, Response},
};

const DOCUMENT: &str = r##"{
  "openapi": "3.1.0",
  "info": {
    "title": "QED verification API",
    "version": "1.0.0",
    "description": "Read-only pool checks, signed attestations, registry data, token-power observations, and verification metadata."
  },
  "paths": {
    "/healthz": { "get": { "summary": "Health", "responses": { "200": { "description": "Service is healthy" } } } },
    "/api/registry": { "get": { "summary": "Issuer registry", "responses": { "200": { "description": "Registry entries" } } } },
    "/api/pools/featured": { "get": { "summary": "Featured pools", "responses": { "200": { "description": "Curated pool list" } } } },
    "/api/leaderboard": { "get": { "summary": "Leaderboard page", "parameters": [{ "name": "page", "in": "query", "schema": { "type": "integer", "minimum": 1 } }, { "name": "per", "in": "query", "schema": { "type": "integer", "minimum": 1, "maximum": 50 } }, { "name": "sort", "in": "query", "schema": { "type": "string", "enum": ["volume", "price", "change", "liquidity"] } }, { "name": "dir", "in": "query", "schema": { "type": "string", "enum": ["asc", "desc"] } }], "responses": { "200": { "description": "Ranked leaderboard page" } } } },
    "/api/prices": { "get": { "summary": "Pool prices", "parameters": [{ "name": "ids", "in": "query", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "Price snapshot" } } } },
    "/api/status": { "get": { "summary": "Freshness status", "responses": { "200": { "description": "Service status" } } } },
    "/api/check/{address}": { "get": { "summary": "Check a pool or token", "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "Check result" } } } },
    "/api/powers/{address}": {
      "get": {
        "summary": "Observed token powers for registered issuer contracts",
        "parameters": [
          { "name": "address", "in": "path", "required": true, "schema": { "type": "string" } },
          { "name": "chain", "in": "query", "required": false, "schema": { "type": "string", "enum": ["solana", "robinhood", "base", "ethereum", "bnb"] }, "description": "Optional chain filter; without it, all active registry matches are returned." }
        ],
        "responses": {
          "200": { "description": "One powers record or an array when the address has active registry matches on multiple chains", "content": { "application/json": { "schema": { "oneOf": [{ "$ref": "#/components/schemas/PowersRecord" }, { "type": "array", "items": { "$ref": "#/components/schemas/PowersRecord" } }] } } } },
          "400": { "description": "Invalid address or chain" },
          "404": { "description": "No registered issuer contract" },
          "502": { "description": "Upstream read failed" }
        }
      }
    },
    "/api/wallet": { "post": { "summary": "Check stock-token holdings", "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object", "required": ["address"], "properties": { "address": { "type": "string" } } } } } }, "responses": { "200": { "description": "Known stock-token holdings and contract verdicts" }, "404": { "description": "Unsupported or invalid address" } } } },
    "/api/attest/{id}": { "get": { "summary": "Fetch a signed attestation", "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string", "pattern": "^[0-9a-fA-F]{64}$" } }], "responses": { "200": { "description": "Attestation JSON" }, "404": { "description": "Not found" } } } },
    "/verify": { "post": { "summary": "Verify an attestation", "requestBody": { "required": true, "content": { "application/json": { "schema": { "type": "object" } } } }, "responses": { "200": { "description": "Five verification booleans" } } } },
    "/mcp": { "post": { "summary": "MCP tools (2026-07-28; legacy 2025-11-25, 2025-06-18, 2025-03-26 initialize)", "description": "Supports MCP versions [\"2026-07-28\", \"2025-11-25\", \"2025-06-18\", \"2025-03-26\"] statelessly. Modern calls use per-request metadata and server/discover; legacy initialize supports all three 2025 revisions.", "requestBody": { "required": true, "content": { "application/json": { "schema": { "oneOf": [{ "$ref": "#/components/schemas/JsonRpcRequest" }, { "$ref": "#/components/schemas/JsonRpcNotification" }] } } } }, "responses": { "200": { "description": "JSON-RPC 2.0 response", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/JsonRpcResponse" } } } }, "202": { "description": "Accepted notification; empty response" }, "400": { "description": "Malformed request or invalid protocol metadata", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/JsonRpcErrorResponse" } } } }, "403": { "description": "Origin does not match request host" }, "404": { "description": "Unknown JSON-RPC method", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/JsonRpcErrorResponse" } } } }, "405": { "description": "POST is required" }, "413": { "description": "Request body exceeds the HTTP limit" } } } },
    "/.well-known/qed.json": { "get": { "summary": "Public verification key", "responses": { "200": { "description": "Ed25519 key metadata" } } } },
    "/.well-known/mcp/server-card.json": { "get": { "summary": "MCP server card", "responses": { "200": { "description": "Read-only MCP server and tool summary", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/McpServerCard" } } } } } } }
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
          "result": {
            "type": "object",
            "description": "MCP CallToolResult; qed_powers structuredContent is a PowersRecord for one match or an object with records (PowersRecordSet) for multiple matches.",
            "properties": {
              "structuredContent": { "description": "Tool-specific structured JSON; see PowersRecord and PowersRecordSet for qed_powers." },
              "content": { "type": "array", "items": { "type": "object" } },
              "isError": { "type": "boolean" }
            }
          }
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
      },
      "PowerReason": {
        "type": "object",
        "required": ["code", "detail"],
        "properties": {
          "code": { "type": "string" },
          "detail": { "type": "string" }
        }
      },
      "ObservedRead": {
        "type": "object",
        "required": ["method", "params", "result_hash", "block", "slot"],
        "properties": {
          "method": { "type": "string" },
          "params": {},
          "result_hash": { "type": "string" },
          "raw_result": {},
          "block": { "type": ["integer", "null"], "minimum": 0 },
          "slot": { "type": ["integer", "null"], "minimum": 0 }
        }
      },
      "PowersRecord": {
        "type": "object",
        "required": ["chain", "contract", "can_seize", "can_block", "can_change_rules", "unavailable", "source_verified_subject", "source_verified", "observed_at", "block", "slot", "reads"],
        "properties": {
          "chain": { "type": "string", "enum": ["Solana", "RobinhoodChain", "Base", "Ethereum", "Bnb"] },
          "contract": { "type": "string" },
          "can_seize": { "type": "array", "items": { "$ref": "#/components/schemas/PowerReason" } },
          "can_block": { "type": "array", "items": { "$ref": "#/components/schemas/PowerReason" } },
          "can_change_rules": { "type": "array", "items": { "$ref": "#/components/schemas/PowerReason" } },
          "unavailable": { "type": "array", "items": { "$ref": "#/components/schemas/PowerReason" } },
          "source_verified_subject": { "type": "string", "enum": ["token_program", "contract", "implementation"], "description": "OSEC verifies the Solana Token-2022 token-program build; Sourcify checks EVM contract source or a proxy's resolved implementation." },
          "source_verified": { "type": "string", "enum": ["exact_match", "match", "none", "unavailable"] },
          "source_verified_proxy": { "type": "string", "enum": ["exact_match", "match", "none", "unavailable"], "description": "Separate Sourcify source status for an EVM proxy, when applicable." },
          "observed_at": { "type": "string", "format": "date-time" },
          "block": { "type": ["integer", "null"], "minimum": 0 },
          "slot": { "type": ["integer", "null"], "minimum": 0 },
          "reads": { "type": "array", "items": { "$ref": "#/components/schemas/ObservedRead" } }
        }
      },
      "PowersRecordSet": {
        "type": "object",
        "required": ["records"],
        "properties": {
          "records": { "type": "array", "items": { "$ref": "#/components/schemas/PowersRecord" } }
        }
      },

      "McpServerCard": {
        "type": "object",
        "required": ["name", "description", "serverInfo", "remotes", "tools", "website", "repository", "readOnly", "readOnlyStatement"],
        "properties": {
          "name": { "type": "string" },
          "description": { "type": "string" },
          "serverInfo": {
            "type": "object",
            "required": ["name", "version"],
            "properties": { "name": { "type": "string" }, "version": { "type": "string" } }
          },
          "remotes": {
            "type": "array",
            "items": {
              "type": "object",
              "required": ["type", "url"],
              "properties": {
                "type": { "const": "streamable-http" },
                "url": { "type": "string", "format": "uri" }
              }
            }
          },
          "tools": {
            "type": "array",
            "items": {
              "type": "object",
              "required": ["name", "title", "description"],
              "properties": {
                "name": { "type": "string" },
                "title": { "type": "string" },
                "description": { "type": "string" }
              }
            }
          },
          "website": { "type": "string", "format": "uri" },
          "repository": { "type": "string", "format": "uri" },
          "readOnly": { "type": "boolean", "const": true },
          "readOnlyStatement": { "type": "string" }
        }
      }
    }
  }
}"##;

pub(crate) async fn document() -> Response {
    ([(header::CONTENT_TYPE, "application/json; charset=utf-8")], DOCUMENT).into_response()
}

#[cfg(test)]
mod tests {
    use super::DOCUMENT;
    use serde_json::Value;

    #[test]
    fn openapi_documents_mcp_powers_and_server_card_contracts() {
        let document: Value = serde_json::from_str(DOCUMENT).expect("valid OpenAPI JSON");
        let paths = &document["paths"];
        let powers_get = &paths["/api/powers/{address}"]["get"];
        assert!(powers_get.is_object());
        assert_eq!(powers_get["parameters"][1]["name"], "chain");
        assert_eq!(
            powers_get["responses"]["200"]["content"]["application/json"]["schema"]["oneOf"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            document["components"]["schemas"]["PowersRecord"]["properties"]["source_verified_subject"]["enum"],
            serde_json::json!(["token_program", "contract", "implementation"])
        );
        assert_eq!(
            document["components"]["schemas"]["PowersRecordSet"]["required"],
            serde_json::json!(["records"])
        );
        assert_eq!(
            document["components"]["schemas"]["PowersRecordSet"]["properties"]["records"]["items"]["$ref"],
            "#/components/schemas/PowersRecord"
        );
        assert!(
            document["components"]["schemas"]["JsonRpcResultResponse"]["properties"]["result"]["description"]
                .as_str()
                .unwrap()
                .contains("PowersRecordSet")
        );
        assert_eq!(
            paths["/.well-known/mcp/server-card.json"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/McpServerCard"
        );
        let mcp_post = &paths["/mcp"]["post"];
        assert!(mcp_post["requestBody"]["content"]["application/json"]["schema"]["oneOf"].is_array());
        assert_eq!(
            mcp_post["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/JsonRpcResponse"
        );
    }
}
