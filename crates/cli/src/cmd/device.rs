//! Device credential management.
use super::Context;
use crate::output::{print_json, print_table};
use anyhow::{Result, bail};
use clap::Subcommand;
use crossagents_api_types::{
    CreateDeviceRequest, CreateDeviceResponse, Device, ListDevicesResponse,
};
#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    Add {
        name: String,
        #[arg(long, default_value="write", value_parser=["read","write","admin"])]
        level: String,
        #[arg(long, default_value="client", value_parser=["client","node"])]
        kind: String,
    },
    List,
    Revoke {
        id: String,
    },
}
pub async fn run(ctx: &Context, cmd: DeviceCommand) -> Result<()> {
    let client = ctx.client()?;
    match cmd {
        DeviceCommand::Add { name, kind, level } => {
            let req = CreateDeviceRequest {
                name,
                kind: Some(serde_json::from_value(kind.into())?),
                level: Some(serde_json::from_value(level.into())?),
            };
            let created: CreateDeviceResponse = client.post("/v1/devices", &req).await?;
            if ctx.json {
                return print_json(&created);
            }
            println!(
                "added device {} ({})",
                created.device.name, created.device.id
            );
            println!("Token (shown only once; save it now): {}", created.token);
        }
        DeviceCommand::List => {
            let list: ListDevicesResponse = client.get("/v1/devices").await?;
            if ctx.json {
                return print_json(&list);
            }
            let rows = list
                .devices
                .iter()
                .map(|d| {
                    vec![
                        d.id.clone(),
                        d.name.clone(),
                        format!("{:?}", d.kind).to_lowercase(),
                        format!("{:?}", d.level).to_lowercase(),
                        d.revoked_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
                    ]
                })
                .collect::<Vec<_>>();
            print_table(&["ID", "NAME", "KIND", "LEVEL", "REVOKED"], &rows);
        }
        DeviceCommand::Revoke { id } => {
            let list: ListDevicesResponse = client.get("/v1/devices").await?;
            let device = if let Some(d) = list.devices.iter().find(|d| d.id == id) {
                d
            } else {
                let matching = list
                    .devices
                    .iter()
                    .filter(|d| d.name == id)
                    .collect::<Vec<_>>();
                if let Some(d) = matching.iter().find(|d| d.revoked_at.is_none()) {
                    d
                } else if matching.len() == 1 {
                    matching[0]
                } else {
                    bail!("device {id:?} not found or ambiguous; use its id");
                }
            };
            let revoked: Device = client
                .delete_json(&format!("/v1/devices/{}", device.id))
                .await?;
            if ctx.json {
                return print_json(&revoked);
            }
            println!("revoked device {} ({})", revoked.name, revoked.id);
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Command};
    use clap::Parser;
    #[test]
    fn device_arguments() {
        let cli =
            Cli::try_parse_from(["crossagents", "device", "add", "laptop", "--json"]).unwrap();
        assert!(cli.json);
        assert!(
            matches!(cli.command, Command::Device(DeviceCommand::Add { level, kind, .. }) if level == "write" && kind == "client")
        );
        for level in ["read", "write", "admin"] {
            for kind in ["client", "node"] {
                assert!(
                    Cli::try_parse_from([
                        "crossagents",
                        "device",
                        "add",
                        "x",
                        "--level",
                        level,
                        "--kind",
                        kind
                    ])
                    .is_ok()
                );
            }
        }
        for args in [
            vec!["crossagents", "device", "list", "--json"],
            vec!["crossagents", "device", "revoke", "laptop"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
        assert!(
            Cli::try_parse_from(["crossagents", "device", "add", "x", "--level", "root"]).is_err()
        );
        assert!(
            Cli::try_parse_from(["crossagents", "device", "add", "x", "--kind", "server"]).is_err()
        );
    }
}
