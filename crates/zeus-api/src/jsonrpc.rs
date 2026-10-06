//! JSON-RPC 2.0 envelope types.
//!
//! Zeus uses the request/response/notification forms of the JSON-RPC 2.0 specification. **Batches are not
//! supported** in protocol version 1: a line that holds a JSON array is answered with `api.invalid_request`.
//! Response `id`s are echoed exactly. Requests are processed concurrently and responses may arrive out of order.

use schemars::JsonSchema;
use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeus_types::ErrorEnvelope;

use crate::error::{ApiError, codes};

/// The literal `"2.0"` that every JSON-RPC 2.0 message carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Version2;

impl Serialize for Version2 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for Version2 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text == "2.0" {
            Ok(Self)
        } else {
            Err(de::Error::custom("jsonrpc must be \"2.0\""))
        }
    }
}

impl JsonSchema for Version2 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "JsonRpcVersion".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "const": "2.0" })
    }
}

/// A request id: an integer or a string, chosen by the requester and unique per connection (00 section 5.3).
/// `null` ids are not used by Zeus.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(untagged)]
pub enum RequestId {
    /// Integer id, within +/- (2^53 - 1).
    Number(i64),
    /// String id of at most 64 characters.
    Text(String),
}

impl From<i64> for RequestId {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
}

/// A request: expects exactly one [`Response`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Request {
    /// Always `"2.0"`.
    pub jsonrpc: Version2,
    /// Correlation id.
    pub id: RequestId,
    /// Dotted method name, for example `jobs.enqueue`.
    pub method: String,
    /// Parameters: an object (positional parameters are not used).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// Build a request.
    pub fn new(id: impl Into<RequestId>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: Version2,
            id: id.into(),
            method: method.into(),
            params,
        }
    }

    /// The parameters, or an empty object when absent.
    pub fn params_or_empty(&self) -> Value {
        self.params
            .clone()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
    }
}

/// A notification: no id, no response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Notification {
    /// Always `"2.0"`.
    pub jsonrpc: Version2,
    /// Method name, for example `events.event`.
    pub method: String,
    /// Parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// Build a notification.
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: Version2,
            method: method.into(),
            params,
        }
    }
}

/// The error member of a failed response. `data` always carries the 00 error envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorObject {
    /// JSON-RPC numeric code ([`crate::error::rpc_code`]).
    pub code: i32,
    /// English diagnostic, equal to `data.message`.
    pub message: String,
    /// The envelope. Optional on the wire only to tolerate foreign peers; Zeus always sends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ErrorEnvelope>,
}

/// A response: `result` on success, `error` on failure, never both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: Version2,
    /// The request's id. `null` only when the request id could not be read (parse errors).
    pub id: Option<RequestId>,
    /// Success value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
}

impl Response {
    /// A success response.
    pub fn success(id: RequestId, result: Value) -> Self {
        Self {
            jsonrpc: Version2,
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }

    /// A failure response. `id` is `None` when the request could not be parsed far enough to read it.
    pub fn failure(id: Option<RequestId>, error: &ApiError) -> Self {
        Self {
            jsonrpc: Version2,
            id,
            result: None,
            error: Some(error.to_error_object()),
        }
    }

    /// Convert into the call result: the value, or the error.
    pub fn into_result(self) -> Result<Value, ApiError> {
        match (self.result, self.error) {
            (Some(value), None) => Ok(value),
            (None, Some(error)) => Err(ApiError::from_error_object(&error)),
            (None, None) => Ok(Value::Null),
            (Some(_), Some(_)) => Err(ApiError::new(
                codes::INVALID_REQUEST,
                "response has both result and error",
            )),
        }
    }
}

/// Any message that can appear on a line.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A request.
    Request(Request),
    /// A notification.
    Notification(Notification),
    /// A response.
    Response(Response),
}

impl Message {
    /// Parse one line (without the newline).
    ///
    /// Errors carry the `id` that could be recovered, so a server can still answer a malformed request.
    pub fn parse(line: &[u8]) -> Result<Self, (Option<RequestId>, ApiError)> {
        let value: Value = serde_json::from_slice(line).map_err(|_| {
            (
                None,
                ApiError::new(codes::PARSE_ERROR, "the line is not valid JSON"),
            )
        })?;
        if !value.is_object() {
            return Err((
                None,
                ApiError::new(
                    codes::INVALID_REQUEST,
                    "expected a JSON object (batches are not supported)",
                ),
            ));
        }
        let id = value
            .get("id")
            .and_then(|v| serde_json::from_value::<RequestId>(v.clone()).ok());
        let invalid = |what: &str| {
            (
                id.clone(),
                ApiError::new(codes::INVALID_REQUEST, format!("invalid {what}")),
            )
        };
        if value.get("method").is_some() {
            if value.get("id").is_some() {
                serde_json::from_value(value)
                    .map(Self::Request)
                    .map_err(|_| invalid("request"))
            } else {
                serde_json::from_value(value)
                    .map(Self::Notification)
                    .map_err(|_| invalid("notification"))
            }
        } else if value.get("result").is_some() || value.get("error").is_some() {
            serde_json::from_value(value)
                .map(Self::Response)
                .map_err(|_| invalid("response"))
        } else {
            Err(invalid("message"))
        }
    }

    /// Serialise to compact JSON (no newline).
    pub fn to_vec(&self) -> Result<Vec<u8>, serde_json::Error> {
        match self {
            Self::Request(m) => serde_json::to_vec(m),
            Self::Notification(m) => serde_json::to_vec(m),
            Self::Response(m) => serde_json::to_vec(m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_the_three_message_kinds() {
        let request = Message::parse(
            br#"{"jsonrpc":"2.0","id":1,"method":"jobs.get","params":{"job_id":"x"}}"#,
        )
        .unwrap();
        assert!(matches!(
            request,
            Message::Request(Request {
                id: RequestId::Number(1),
                ..
            })
        ));
        let note =
            Message::parse(br#"{"jsonrpc":"2.0","method":"events.event","params":{}}"#).unwrap();
        assert!(matches!(note, Message::Notification(_)));
        let response =
            Message::parse(br#"{"jsonrpc":"2.0","id":"a","result":{"ok":true}}"#).unwrap();
        assert!(matches!(response, Message::Response(_)));
    }

    #[test]
    fn rejects_malformed_input_with_the_right_code() {
        let (id, error) = Message::parse(b"{nope").unwrap_err();
        assert!(id.is_none());
        assert_eq!(error.envelope().code.as_str(), codes::PARSE_ERROR);

        let (_, error) = Message::parse(b"[]").unwrap_err();
        assert_eq!(error.envelope().code.as_str(), codes::INVALID_REQUEST);

        let (id, error) = Message::parse(br#"{"jsonrpc":"1.0","id":7,"method":"x"}"#).unwrap_err();
        assert_eq!(id, Some(RequestId::Number(7)));
        assert_eq!(error.envelope().code.as_str(), codes::INVALID_REQUEST);

        assert!(Message::parse(br#"{"jsonrpc":"2.0","id":1}"#).is_err());
    }

    #[test]
    fn responses_serialise_with_exactly_one_member() {
        let ok =
            serde_json::to_value(Response::success(RequestId::Number(3), json!({"a": 1}))).unwrap();
        assert!(ok.get("result").is_some() && ok.get("error").is_none());
        let err = serde_json::to_value(Response::failure(
            Some(RequestId::Number(3)),
            &ApiError::not_authenticated(),
        ))
        .unwrap();
        assert!(err.get("error").is_some() && err.get("result").is_none());
        assert_eq!(err["error"]["data"]["class"], "unauthenticated");
    }

    #[test]
    fn into_result_round_trips_errors() {
        let error = ApiError::invalid_params("bad");
        let response = Response::failure(Some(RequestId::Text("r1".into())), &error);
        assert_eq!(response.into_result().unwrap_err(), error);
    }
}
