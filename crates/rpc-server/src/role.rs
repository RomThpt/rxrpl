use std::net::IpAddr;

use rxrpl_config::ServerConfig;

/// Connection role determined by the client's IP address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionRole {
    Admin,
    Public,
}

impl ConnectionRole {
    /// Determine the role from the connecting IP and server configuration.
    ///
    /// Localhost (127.0.0.1, ::1) is always admin. Additional admin IPs
    /// can be configured via `ServerConfig::admin_ips`. The strings
    /// `"0.0.0.0"` and `"::"` are rippled-style sentinels that grant admin
    /// to any IPv4 / IPv6 client respectively.
    pub fn from_ip(ip: IpAddr, config: &ServerConfig) -> Self {
        if ip.is_loopback() {
            return Self::Admin;
        }
        let ip_str = ip.to_string();
        for entry in &config.admin_ips {
            if entry == &ip_str {
                return Self::Admin;
            }
            if entry == "0.0.0.0" && ip.is_ipv4() {
                return Self::Admin;
            }
            if entry == "::" && ip.is_ipv6() {
                return Self::Admin;
            }
        }
        Self::Public
    }

    pub fn is_admin(self) -> bool {
        self == Self::Admin
    }
}

/// Request context carrying role and API version for a single request.
#[derive(Clone, Debug)]
pub struct RequestContext {
    pub role: ConnectionRole,
    pub api_version: rxrpl_rpc_api::ApiVersion,
}

/// Returns `true` if `method` requires admin privileges.
pub fn is_admin_method(method: &str) -> bool {
    matches!(
        method,
        "stop"
            | "log_level"
            | "connect"
            | "logrotate"
            | "can_delete"
            | "validation_create"
            | "validation_seed"
            | "validator_info"
            | "crawl"
            | "crawl_shards"
            | "tx_reduce_relay"
            | "ledger_accept"
            | "ledger_cleaner"
            | "peers"
            | "peer_reservations_add"
            | "peer_reservations_del"
            | "peer_reservations_list"
            | "print"
            | "fetch_info"
            | "consensus_info"
            | "server_subscribe"
            | "validators"
            | "validator_list_sites"
            | "ledger_diff"
            | "get_counts"
            | "unl_list"
            | "download_shard"
            | "node_to_shard"
            | "shard_info"
            | "wallet_seed"
            | "internal"
            | "blacklist"
            | "metrics"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ServerConfig {
        ServerConfig::default()
    }

    #[test]
    fn loopback_ipv4_is_admin() {
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(
            ConnectionRole::from_ip(ip, &default_config()),
            ConnectionRole::Admin
        );
    }

    #[test]
    fn loopback_ipv6_is_admin() {
        let ip: IpAddr = "::1".parse().unwrap();
        assert_eq!(
            ConnectionRole::from_ip(ip, &default_config()),
            ConnectionRole::Admin
        );
    }

    #[test]
    fn configured_admin_ip_is_admin() {
        let mut config = default_config();
        config.admin_ips.push("10.0.0.5".into());
        let ip: IpAddr = "10.0.0.5".parse().unwrap();
        assert_eq!(ConnectionRole::from_ip(ip, &config), ConnectionRole::Admin);
    }

    #[test]
    fn random_ip_is_public() {
        let ip: IpAddr = "203.0.113.42".parse().unwrap();
        assert_eq!(
            ConnectionRole::from_ip(ip, &default_config()),
            ConnectionRole::Public
        );
    }

    #[test]
    fn wildcard_ipv4_grants_admin_to_external() {
        let mut config = default_config();
        config.admin_ips.push("0.0.0.0".into());
        let ip: IpAddr = "172.18.0.5".parse().unwrap();
        assert_eq!(ConnectionRole::from_ip(ip, &config), ConnectionRole::Admin);
    }

    #[test]
    fn wildcard_ipv6_grants_admin_to_external() {
        let mut config = default_config();
        config.admin_ips.push("::".into());
        let ip: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(ConnectionRole::from_ip(ip, &config), ConnectionRole::Admin);
    }

    #[test]
    fn wildcard_ipv4_does_not_match_ipv6() {
        let mut config = default_config();
        config.admin_ips.push("0.0.0.0".into());
        let ip: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(ConnectionRole::from_ip(ip, &config), ConnectionRole::Public);
    }

    #[test]
    fn admin_method_stop() {
        assert!(is_admin_method("stop"));
    }

    #[test]
    fn public_method_ping() {
        assert!(!is_admin_method("ping"));
    }

    #[test]
    fn admin_method_ledger_accept() {
        assert!(is_admin_method("ledger_accept"));
    }

    #[test]
    fn public_method_account_info() {
        assert!(!is_admin_method("account_info"));
    }
}
