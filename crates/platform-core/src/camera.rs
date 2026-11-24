//! The camera interface name, chosen by the platform backend.
//!
//! The glue in `platform` owns the camera code and only needs to know which
//! interface to open; naming it here keeps that choice out of the glue.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CameraApi {
    MediaFoundation,
    Video4Linux,
}
