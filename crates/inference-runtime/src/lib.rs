//! The inference-runtime pod: a gRPC SERVER the harness dials for one model
//! call. The harness opens a bidirectional `InferenceJob.Run` stream, sends the
//! model-call assignment as the first `RunInferenceCommand`, and the pod runs
//! the provider call and streams the model's `TurnEvent`s back on the same held
//! connection. The pod holds no client that dials out to the harness.

pub mod server;

pub use server::InferenceJobService;
