use crate::cert::DnsProvider;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;

mod config;
pub use config::NamecheapConfig;

const BASE_URL: &str = "https://api.namecheap.com/xml.response";

pub struct NamecheapDns {
    api_user: String,
    api_key: String,
    client_ip: String,
    client: reqwest::Client,
}

#[derive(Debug, Default, Clone)]
struct HostRecord {
    name: String,
    record_type: String,
    address: String,
    ttl: String,
    mx_pref: String,
}

impl NamecheapDns {
    pub fn new(config: NamecheapConfig) -> Self {
        Self {
            api_user: config.api_user,
            api_key: config.api_key,
            client_ip: config.client_ip,
            client: reqwest::Client::new(),
        }
    }

    fn base_params(&self) -> Vec<(&str, &str)> {
        vec![
            ("ApiUser", &self.api_user),
            ("ApiKey", &self.api_key),
            ("UserName", &self.api_user),
            ("ClientIp", &self.client_ip),
        ]
    }

    fn split_domain(domain: &str) -> (&str, &str) {
        domain.split_once('.').unwrap_or((domain, ""))
    }

    async fn find_domain(&self, name: &str) -> Result<(String, String, String)> {
        let mut params = self.base_params();
        params.push(("Command", "namecheap.domains.getList"));
        params.push(("PageSize", "100"));

        let xml = self
            .client
            .get(BASE_URL)
            .query(&params)
            .send()
            .await
            .context("Failed to list Namecheap domains")?
            .text()
            .await
            .context("Failed to read Namecheap domain list response")?;

        let domain_names = parse_domain_list(&xml)?;
        let labels: Vec<&str> = name.split('.').collect();

        for i in 1..labels.len() {
            let candidate = labels[i..].join(".");
            let relative = labels[..i].join(".");

            if domain_names.contains(&candidate) {
                let (sld, tld) = Self::split_domain(&candidate);
                return Ok((sld.to_string(), tld.to_string(), relative));
            }
        }

        bail!("No Namecheap domain found for: {name}")
    }

    async fn get_hosts(&self, sld: &str, tld: &str) -> Result<Vec<HostRecord>> {
        let mut params = self.base_params();
        params.push(("Command", "namecheap.domains.dns.getHosts"));
        let sld_param = sld.to_string();
        let tld_param = tld.to_string();
        params.push(("SLD", &sld_param));
        params.push(("TLD", &tld_param));

        let xml = self
            .client
            .get(BASE_URL)
            .query(&params)
            .send()
            .await
            .context("Failed to get Namecheap DNS hosts")?
            .text()
            .await
            .context("Failed to read Namecheap getHosts response")?;

        parse_hosts(&xml)
    }

    async fn set_hosts(&self, sld: &str, tld: &str, hosts: &[HostRecord]) -> Result<()> {
        let mut params: Vec<(String, String)> = vec![
            ("ApiUser".into(), self.api_user.clone()),
            ("ApiKey".into(), self.api_key.clone()),
            ("UserName".into(), self.api_user.clone()),
            ("ClientIp".into(), self.client_ip.clone()),
            ("Command".into(), "namecheap.domains.dns.setHosts".into()),
            ("SLD".into(), sld.to_string()),
            ("TLD".into(), tld.to_string()),
        ];

        for (i, host) in hosts.iter().enumerate() {
            let n = i + 1;
            params.push((format!("HostName{n}"), host.name.clone()));
            params.push((format!("RecordType{n}"), host.record_type.clone()));
            params.push((format!("Address{n}"), host.address.clone()));
            params.push((format!("TTL{n}"), host.ttl.clone()));
            if !host.mx_pref.is_empty() && host.record_type == "MX" {
                params.push((format!("MXPref{n}"), host.mx_pref.clone()));
            }
        }

        self.client
            .get(BASE_URL)
            .query(&params)
            .send()
            .await
            .context("Failed to set Namecheap DNS hosts")?
            .error_for_status()
            .context("Namecheap setHosts request failed")?;

        Ok(())
    }
}

#[async_trait]
impl DnsProvider for NamecheapDns {
    async fn create_txt_record(&self, name: &str, value: &str) -> Result<String> {
        let (sld, tld, relative_name) = self.find_domain(name).await?;

        let mut hosts = self.get_hosts(&sld, &tld).await?;
        hosts.push(HostRecord {
            name: relative_name.clone(),
            record_type: "TXT".into(),
            address: value.to_string(),
            ttl: "60".into(),
            mx_pref: String::new(),
        });

        self.set_hosts(&sld, &tld, &hosts).await?;

        Ok(format!("{sld}.{tld}|{relative_name}|{value}"))
    }

    async fn delete_txt_record(&self, record_id: &str) -> Result<()> {
        let parts: Vec<&str> = record_id.splitn(3, '|').collect();
        if parts.len() != 3 {
            bail!("Invalid Namecheap record_id format, expected '<domain>|<host_name>|<value>'");
        }
        let (domain, host_name, value) = (parts[0], parts[1], parts[2]);

        let (sld, tld) = NamecheapDns::split_domain(domain);
        let mut hosts = self.get_hosts(sld, tld).await?;

        hosts.retain(|h| !(h.name == host_name && h.record_type == "TXT" && h.address == value));

        self.set_hosts(sld, tld, &hosts).await
    }
}

fn parse_domain_list(xml: &str) -> Result<Vec<String>> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    let mut domains = Vec::new();

    loop {
        match reader
            .read_event()
            .context("Failed to parse Namecheap domain list XML")?
        {
            Event::Empty(e) | Event::Start(e) if e.local_name().as_ref() == b"Domain" => {
                for attr in e.attributes().flatten() {
                    if attr.key.local_name().as_ref() == b"Name" {
                        let val = String::from_utf8_lossy(&attr.value).to_string();
                        domains.push(val);
                        break;
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(domains)
}

fn parse_hosts(xml: &str) -> Result<Vec<HostRecord>> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    let mut hosts = Vec::new();

    loop {
        match reader
            .read_event()
            .context("Failed to parse Namecheap getHosts XML")?
        {
            Event::Empty(e) | Event::Start(e) if e.local_name().as_ref() == b"host" => {
                let mut record = HostRecord::default();
                for attr in e.attributes().flatten() {
                    let val = String::from_utf8_lossy(&attr.value).to_string();
                    match attr.key.local_name().as_ref() {
                        b"Name" => record.name = val,
                        b"Type" => record.record_type = val,
                        b"Address" => record.address = val,
                        b"TTL" => record.ttl = val,
                        b"MXPref" => record.mx_pref = val,
                        _ => {}
                    }
                }
                hosts.push(record);
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(hosts)
}
