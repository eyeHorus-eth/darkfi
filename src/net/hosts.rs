/* This file is part of DarkFi (https://dark.fi)
 *
 * Copyright (C) 2020-2023 Dyne.org foundation
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as
 * published by the Free Software Foundation, either version 3 of the
 * License, or (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU Affero General Public License for more details.
 *
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use log::debug;
use rand::{prelude::IteratorRandom, rngs::OsRng};
use smol::lock::RwLock;
use url::Url;

use super::settings::SettingsPtr;
use crate::{
    system::{Subscriber, SubscriberPtr, Subscription},
    Result,
};

/// Atomic pointer to hosts object
pub type HostsPtr = Arc<Hosts>;

// An array containing all possible local host strings
// TODO: This could perhaps be more exhaustive?
pub const LOCAL_HOST_STRS: [&str; 2] = ["localhost", "localhost.localdomain"];

/// Manages a store of network addresses
pub struct Hosts {
    /// Set of stored addresses
    addrs: RwLock<HashSet<Url>>,

    /// Set of stored addresses that are quarantined.
    /// We quarantine peers we've been unable to connect to, but we keep them
    /// around so we can potentially try them again, up to n tries. This should
    /// be helpful in order to self-heal the p2p connections in case we have an
    /// Internet interrupt (goblins unplugging cables)
    quarantine: RwLock<HashMap<Url, usize>>,

    /// Peers we reject from connecting
    rejected: RwLock<HashSet<String>>,

    /// Subscriber listening for store updates
    store_subscriber: SubscriberPtr<usize>,

    /// Pointer to configured P2P settings
    settings: SettingsPtr,
}

impl Hosts {
    /// Create a new hosts list>
    pub fn new(settings: SettingsPtr) -> HostsPtr {
        Arc::new(Self {
            addrs: RwLock::new(HashSet::new()),
            quarantine: RwLock::new(HashMap::new()),
            rejected: RwLock::new(HashSet::new()),
            store_subscriber: Subscriber::new(),
            settings,
        })
    }

    /// Append given addrs to the known set.
    pub async fn store(&self, addrs: &[Url]) {
        debug!(target: "net::hosts::store()", "hosts::store() [START]");

        let filtered_addrs = self.filter_addresses(addrs).await;
        let filtered_addrs_len = filtered_addrs.len();

        if !filtered_addrs.is_empty() {
            let mut addrs_map = self.addrs.write().await;
            for addr in filtered_addrs {
                debug!(target: "net::hosts::store()", "Inserting {}", addr);
                addrs_map.insert(addr);
            }
        }

        self.store_subscriber.notify(filtered_addrs_len).await;
        debug!(target: "net::hosts::store()", "hosts::store() [END]");
    }

    pub async fn subscribe_store(&self) -> Result<Subscription<usize>> {
        let sub = self.store_subscriber.clone().subscribe().await;
        Ok(sub)
    }

    // Verify whether a URL is local.
    // NOTE: This function is stateless and not specific to
    // `Hosts`. For this reason, it might make more sense
    // to move this function to a more appropriate location
    // in the codebase.
    pub async fn is_local_host(&self, url: Url) -> bool {
        // Reject Urls without host strings.
        if url.host_str().is_none() {
            return false
        }

        // We do this hack in order to parse IPs properly.
        // https://github.com/whatwg/url/issues/749
        let addr = Url::parse(&url.as_str().replace(url.scheme(), "http")).unwrap();
        // Filter private IP ranges
        match addr.host().unwrap() {
            url::Host::Ipv4(ip) => {
                if !ip.is_global() {
                    return true
                }
            }
            url::Host::Ipv6(ip) => {
                if !ip.is_global() {
                    return true
                }
            }
            url::Host::Domain(d) => {
                if LOCAL_HOST_STRS.contains(&d) {
                    return true
                }
            }
        }
        false
    }

    /// Filter given addresses based on certain rulesets and validity.
    async fn filter_addresses(&self, addrs: &[Url]) -> Vec<Url> {
        debug!(target: "net::hosts::filter_addresses()", "Filtering addrs: {:?}", addrs);
        let mut ret = vec![];
        let localnet = self.settings.localnet;

        'addr_loop: for addr_ in addrs {
            // Validate that the format is `scheme://host_str:port`
            if addr_.host_str().is_none() ||
                addr_.port().is_none() ||
                addr_.cannot_be_a_base() ||
                addr_.path_segments().is_some()
            {
                continue
            }

            if self.is_rejected(addr_).await {
                debug!(target: "net::hosts::filter_addresses()", "Peer {} is rejected", addr_);
                continue
            }

            let host_str = addr_.host_str().unwrap();

            if !localnet {
                // Our own external addresses should never enter the hosts set.
                for ext in &self.settings.external_addrs {
                    if host_str == ext.host_str().unwrap() {
                        continue 'addr_loop
                    }
                }
            }

            // We do this hack in order to parse IPs properly.
            // https://github.com/whatwg/url/issues/749
            let addr = Url::parse(&addr_.as_str().replace(addr_.scheme(), "http")).unwrap();

            // Filter non-global ranges if we're not allowing localnet.
            // Should never be allowed in production, so we don't really care
            // about some of them (e.g. 0.0.0.0, or broadcast, etc.).
            if !localnet && self.is_local_host(addr).await {
                continue
            }

            match addr_.scheme() {
                // Validate that the address is an actual onion.
                #[cfg(feature = "p2p-tor")]
                "tor" | "tor+tls" => {
                    use std::str::FromStr;
                    if tor_hscrypto::pk::HsId::from_str(host_str).is_err() {
                        continue
                    }
                    debug!(target: "net::hosts::filter_addresses()", "[Tor] Valid: {}", host_str);
                }

                #[cfg(feature = "p2p-nym")]
                "nym" | "nym+tls" => continue, // <-- Temp skip

                #[cfg(feature = "p2p-tcp")]
                "tcp" | "tcp+tls" => {
                    debug!(target: "net::hosts::filter_addresses()", "[TCP] Valid: {}", host_str);
                }

                _ => continue,
            }

            ret.push(addr_.clone());
        }

        ret
    }

    pub async fn remove(&self, url: &Url) {
        debug!(target: "net::hosts::remove()", "Removing peer {}", url);
        self.addrs.write().await.remove(url);
        self.quarantine.write().await.remove(url);
    }

    /// Quarantine a peer.
    /// If they've been quarantined for more than a configured limit, forget them.
    pub async fn quarantine(&self, url: &Url) {
        debug!(target: "net::hosts::remove()", "Quarantining peer {}", url);
        // Remove from main hosts set
        self.addrs.write().await.remove(url);

        let mut q = self.quarantine.write().await;
        if let Some(retries) = q.get_mut(url) {
            *retries += 1;
            debug!(target: "net::hosts::quarantine()", "Peer {} quarantined {} times", url, retries);
            if *retries == self.settings.hosts_quarantine_limit {
                debug!(target: "net::hosts::quarantine()", "Banning peer {}", url);
                q.remove(url);
                self.mark_rejected(url).await;
            }
        } else {
            debug!(target: "net::hosts::remove()", "Added peer {} to quarantine", url);
            q.insert(url.clone(), 0);
        }
    }

    /// Check if a given peer (URL) is in the set of rejected hosts
    pub async fn is_rejected(&self, peer: &Url) -> bool {
        // Skip lookup for UNIX sockets and localhost connections
        // as they should never belong to the list of rejected URLs.
        let Some(hostname) = peer.host_str() else { return false };

        if self.is_local_host(peer.clone()).await {
            return false
        }

        self.rejected.read().await.contains(hostname)
    }

    /// Mark a peer as rejected by adding it to the set of rejected URLs.
    pub async fn mark_rejected(&self, peer: &Url) {
        // We ignore UNIX sockets here so we will just work
        // with stuff that has host_str().
        if let Some(hostname) = peer.host_str() {
            // Localhost connections should not be rejected
            // This however allows any Tor and Nym connections.
            if self.is_local_host(peer.clone()).await {
                return
            }

            self.rejected.write().await.insert(hostname.to_string());
        }
    }

    /// Unmark a rejected peer
    pub async fn unmark_rejected(&self, peer: &Url) {
        if let Some(hostname) = peer.host_str() {
            self.rejected.write().await.remove(hostname);
        }
    }

    /// Check if the host list is empty.
    pub async fn is_empty(&self) -> bool {
        self.addrs.read().await.is_empty()
    }

    /// Check if host is already in the set
    pub async fn contains(&self, addr: &Url) -> bool {
        self.addrs.read().await.contains(addr)
    }

    /// Return all known hosts
    pub async fn fetch_all(&self) -> Vec<Url> {
        self.addrs.read().await.iter().cloned().collect()
    }

    /// Get up to n random peers from the hosts set.
    pub async fn fetch_n_random(&self, n: u32) -> Vec<Url> {
        let n = n as usize;
        if n == 0 {
            return vec![]
        }
        let addrs = self.addrs.read().await;
        let urls = addrs.iter().choose_multiple(&mut OsRng, n.min(addrs.len()));
        urls.iter().map(|&url| url.clone()).collect()
    }

    /// Get up to n random peers that match the given transport schemes from the hosts set.
    pub async fn fetch_n_random_with_schemes(&self, schemes: &[String], n: u32) -> Vec<Url> {
        let n = n as usize;
        if n == 0 {
            return vec![]
        }

        // Retrieve all peers corresponding to that transport schemes
        let hosts = self.fetch_with_schemes(schemes, None).await;
        if hosts.is_empty() {
            return hosts
        }

        // Grab random ones
        let urls = hosts.iter().choose_multiple(&mut OsRng, n.min(hosts.len()));
        urls.iter().map(|&url| url.clone()).collect()
    }

    /// Get up to n random peers that don't match the given transport schemes from the hosts set.
    pub async fn fetch_n_random_excluding_schemes(&self, schemes: &[String], n: u32) -> Vec<Url> {
        let n = n as usize;
        if n == 0 {
            return vec![]
        }

        // Retrieve all peers not corresponding to that transport schemes
        let hosts = self.fetch_exluding_schemes(schemes, None).await;
        if hosts.is_empty() {
            return hosts
        }

        // Grab random ones
        let urls = hosts.iter().choose_multiple(&mut OsRng, n.min(hosts.len()));
        urls.iter().map(|&url| url.clone()).collect()
    }

    /// Get up to limit peers that match the given transport schemes from the hosts set.
    /// If limit was not provided, return all matching peers.
    pub async fn fetch_with_schemes(&self, schemes: &[String], limit: Option<usize>) -> Vec<Url> {
        let addrs = self.addrs.read().await;
        let mut limit = match limit {
            Some(l) => l.min(addrs.len()),
            None => addrs.len(),
        };
        let mut ret = vec![];

        if limit == 0 {
            return ret
        }

        for addr in addrs.iter() {
            if schemes.contains(&addr.scheme().to_string()) {
                ret.push(addr.clone());
                limit -= 1;
                if limit == 0 {
                    return ret
                }
            }
        }

        // If we didn't find any, pick some from the quarantine zone
        if ret.is_empty() {
            for addr in self.quarantine.read().await.keys() {
                if schemes.contains(&addr.scheme().to_string()) {
                    ret.push(addr.clone());
                    limit -= 1;
                    if limit == 0 {
                        break
                    }
                }
            }
        }

        ret
    }

    /// Get up to limit peers that don't match the given transport schemes from the hosts set.
    /// If limit was not provided, return all matching peers.
    pub async fn fetch_exluding_schemes(
        &self,
        schemes: &[String],
        limit: Option<usize>,
    ) -> Vec<Url> {
        let addrs = self.addrs.read().await;
        let mut limit = match limit {
            Some(l) => l.min(addrs.len()),
            None => addrs.len(),
        };
        let mut ret = vec![];

        if limit == 0 {
            return ret
        }

        for addr in addrs.iter() {
            if !schemes.contains(&addr.scheme().to_string()) {
                ret.push(addr.clone());
                limit -= 1;
                if limit == 0 {
                    return ret
                }
            }
        }

        // If we didn't find any, pick some from the quarantine zone
        if ret.is_empty() {
            for addr in self.quarantine.read().await.keys() {
                if !schemes.contains(&addr.scheme().to_string()) {
                    ret.push(addr.clone());
                    limit -= 1;
                    if limit == 0 {
                        break
                    }
                }
            }
        }

        ret
    }
}

#[cfg(test)]
mod tests {
    use super::{super::settings::Settings, *};

    #[test]
    fn test_store_localnet() {
        smol::block_on(async {
            let settings = Settings {
                localnet: true,
                external_addrs: vec![
                    Url::parse("tcp://foo.bar:123").unwrap(),
                    Url::parse("tcp://lol.cat:321").unwrap(),
                ],
                ..Default::default()
            };

            let hosts = Hosts::new(Arc::new(settings.clone()));
            hosts.store(&settings.external_addrs).await;
            for i in settings.external_addrs {
                assert!(hosts.contains(&i).await);
            }

            let local_hosts = vec![
                Url::parse("tcp://localhost:3921").unwrap(),
                Url::parse("tcp://127.0.0.1:23957").unwrap(),
                Url::parse("tcp://[::1]:21481").unwrap(),
                Url::parse("tcp://192.168.10.65:311").unwrap(),
                Url::parse("tcp://0.0.0.0:2312").unwrap(),
                Url::parse("tcp://255.255.255.255:2131").unwrap(),
            ];
            hosts.store(&local_hosts).await;
            for i in local_hosts {
                assert!(hosts.contains(&i).await);
            }

            let remote_hosts = vec![
                Url::parse("tcp://dark.fi:80").unwrap(),
                Url::parse("tcp://top.kek:111").unwrap(),
                Url::parse("tcp://http.cat:401").unwrap(),
            ];
            hosts.store(&remote_hosts).await;
            for i in remote_hosts {
                assert!(hosts.contains(&i).await);
            }
        });
    }

    #[test]
    fn test_store() {
        smol::block_on(async {
            let settings = Settings {
                localnet: false,
                external_addrs: vec![
                    Url::parse("tcp://foo.bar:123").unwrap(),
                    Url::parse("tcp://lol.cat:321").unwrap(),
                ],
                ..Default::default()
            };

            let hosts = Hosts::new(Arc::new(settings.clone()));
            hosts.store(&settings.external_addrs).await;
            assert!(hosts.is_empty().await);

            let local_hosts = vec![
                Url::parse("tcp://localhost:3921").unwrap(),
                Url::parse("tor://[::1]:21481").unwrap(),
                Url::parse("tcp://192.168.10.65:311").unwrap(),
                Url::parse("tcp+tls://0.0.0.0:2312").unwrap(),
                Url::parse("tcp://255.255.255.255:2131").unwrap(),
            ];
            hosts.store(&local_hosts).await;
            assert!(hosts.is_empty().await);

            let remote_hosts = vec![
                Url::parse("tcp://dark.fi:80").unwrap(),
                Url::parse("tcp://http.cat:401").unwrap(),
                Url::parse("tcp://foo.bar:111").unwrap(),
            ];
            hosts.store(&remote_hosts).await;
            assert!(hosts.contains(&remote_hosts[0]).await);
            assert!(hosts.contains(&remote_hosts[1]).await);
            assert!(!hosts.contains(&remote_hosts[2]).await);
        });
    }

    #[test]
    fn test_is_local_host() {
        smol::block_on(async {
            let settings = Settings {
                localnet: false,
                external_addrs: vec![
                    Url::parse("tcp://foo.bar:123").unwrap(),
                    Url::parse("tcp://lol.cat:321").unwrap(),
                ],
                ..Default::default()
            };
            let hosts = Hosts::new(Arc::new(settings.clone()));

            let local_hosts: Vec<Url> = vec![
                Url::parse("tcp://localhost").unwrap(),
                Url::parse("tcp://127.0.0.1").unwrap(),
                Url::parse("tcp+tls://[::1]").unwrap(),
                Url::parse("tcp://localhost.localdomain").unwrap(),
                Url::parse("tcp://192.168.10.65").unwrap(),
            ];
            for host in local_hosts {
                eprintln!("{}", host);
                assert!(hosts.is_local_host(host).await);
            }
            let remote_hosts: Vec<Url> = vec![
                Url::parse("https://dyne.org").unwrap(),
                Url::parse("tcp://77.168.10.65:2222").unwrap(),
                Url::parse("tcp://[2345:0425:2CA1:0000:0000:0567:5673:23b5]").unwrap(),
                Url::parse("http://eweiibe6tdjsdprb4px6rqrzzcsi22m4koia44kc5pcjr7nec2rlxyad.onion")
                    .unwrap(),
            ];
            for host in remote_hosts {
                assert!(!(hosts.is_local_host(host).await))
            }
        });
    }
}
