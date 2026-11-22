//! Every Sonar/GG route string lives here and nowhere else.
//!
//! When GG changes a route, it should be a one-line fix in this file.

/// GG's own API (HTTPS, self-signed): lists the sub-apps, including Sonar's web server address.
pub const GG_SUB_APPS: &str = "/subApps";

/// A read-only GET endpoint that `probe` dumps.
#[derive(Debug, Clone, Copy)]
pub struct ProbeEndpoint {
    /// File-name stem for the dump (`<name>.json`, `<name>.meta.txt`).
    pub name: &'static str,
    /// Path relative to Sonar's base URL.
    pub path: &'static str,
}

/// Sonar GET endpoints that `probe` dumps. All read-only.
pub const PROBE_ENDPOINTS: &[ProbeEndpoint] = &[
    ProbeEndpoint {
        name: "mode",
        path: "/mode/",
    },
    ProbeEndpoint {
        name: "volumeSettings-classic",
        path: "/volumeSettings/classic",
    },
    ProbeEndpoint {
        name: "volumeSettings-streamer",
        path: "/volumeSettings/streamer",
    },
    ProbeEndpoint {
        name: "audioDevices",
        path: "/audioDevices",
    },
    ProbeEndpoint {
        name: "classicRedirections",
        path: "/classicRedirections",
    },
    ProbeEndpoint {
        name: "streamRedirections",
        path: "/streamRedirections",
    },
    ProbeEndpoint {
        name: "streamRedirections-isStreamMonitoringEnabled",
        path: "/streamRedirections/isStreamMonitoringEnabled",
    },
    ProbeEndpoint {
        name: "configs",
        path: "/configs",
    },
    ProbeEndpoint {
        name: "configs-selected",
        path: "/configs/selected",
    },
    ProbeEndpoint {
        name: "chatMix",
        path: "/chatMix",
    },
    ProbeEndpoint {
        name: "v1-chatMix",
        path: "/v1/chatMix",
    },
];
