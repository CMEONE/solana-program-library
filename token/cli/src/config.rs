use crate::Error;
use clap::ArgMatches;
use solana_clap_utils::{
    input_parsers::pubkey_of_signer,
    keypair::{pubkey_from_path, signer_from_path_with_config, SignerFromPathConfig},
};
use solana_cli_output::OutputFormat;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_remote_wallet::remote_wallet::RemoteWalletManager;
use solana_sdk::{pubkey::Pubkey, signature::Signer};
use spl_associated_token_account::*;
use spl_token_2022::{
    extension::StateWithExtensionsOwned,
    state::{Account, Mint},
};
use std::{process::exit, sync::Arc};

#[cfg(test)]
use solana_sdk::signer::keypair::Keypair;

pub(crate) enum KeypairOrPath {
    /// Used for testing environments to avoid touching the filesystem
    #[cfg(test)]
    Keypair(Keypair),
    /// Used for real CLI usage
    Path(String),
}

pub(crate) struct MintInfo {
    pub program_id: Pubkey,
    pub address: Pubkey,
    pub decimals: u8,
}

pub(crate) struct Config<'a> {
    pub(crate) rpc_client: Arc<RpcClient>,
    pub(crate) websocket_url: String,
    pub(crate) output_format: OutputFormat,
    pub(crate) fee_payer: Pubkey,
    pub(crate) default_keypair: KeypairOrPath,
    pub(crate) nonce_account: Option<Pubkey>,
    pub(crate) nonce_authority: Option<Pubkey>,
    pub(crate) sign_only: bool,
    pub(crate) dump_transaction_message: bool,
    pub(crate) multisigner_pubkeys: Vec<&'a Pubkey>,
    pub(crate) program_id: Pubkey,
}

impl<'a> Config<'a> {
    // Check if an explicit token account address was provided, otherwise
    // return the associated token address for the default address.
    pub(crate) async fn associated_token_address_or_override(
        &self,
        arg_matches: &ArgMatches<'_>,
        override_name: &str,
        wallet_manager: &mut Option<Arc<RemoteWalletManager>>,
    ) -> Pubkey {
        let token = pubkey_of_signer(arg_matches, "token", wallet_manager).unwrap();
        self.associated_token_address_for_token_or_override(
            arg_matches,
            override_name,
            wallet_manager,
            token,
        )
        .await
    }

    // Check if an explicit token account address was provided, otherwise
    // return the associated token address for the default address.
    pub(crate) async fn associated_token_address_for_token_or_override(
        &self,
        arg_matches: &ArgMatches<'_>,
        override_name: &str,
        wallet_manager: &mut Option<Arc<RemoteWalletManager>>,
        token: Option<Pubkey>,
    ) -> Pubkey {
        if let Some(address) = pubkey_of_signer(arg_matches, override_name, wallet_manager).unwrap()
        {
            return address;
        }

        let token = token.unwrap();
        let program_id = self.get_mint_info(&token, None).await.unwrap().program_id;
        self.associated_token_address_for_token_and_program(
            arg_matches,
            wallet_manager,
            &token,
            &program_id,
        )
    }

    pub(crate) fn associated_token_address_for_token_and_program(
        &self,
        arg_matches: &ArgMatches<'_>,
        wallet_manager: &mut Option<Arc<RemoteWalletManager>>,
        token: &Pubkey,
        program_id: &Pubkey,
    ) -> Pubkey {
        let owner = self
            .default_address(arg_matches, wallet_manager)
            .unwrap_or_else(|e| {
                eprintln!("error: {}", e);
                exit(1);
            });
        get_associated_token_address_with_program_id(&owner, token, program_id)
    }

    // Checks if an explicit address was provided, otherwise return the default address.
    pub(crate) fn pubkey_or_default(
        &self,
        arg_matches: &ArgMatches,
        address_name: &str,
        wallet_manager: &mut Option<Arc<RemoteWalletManager>>,
    ) -> Pubkey {
        if address_name != "owner" {
            if let Some(address) =
                pubkey_of_signer(arg_matches, address_name, wallet_manager).unwrap()
            {
                return address;
            }
        }

        return self
            .default_address(arg_matches, wallet_manager)
            .unwrap_or_else(|e| {
                eprintln!("error: {}", e);
                exit(1);
            });
    }

    // Checks if an explicit signer was provided, otherwise return the default signer.
    pub(crate) fn signer_or_default(
        &self,
        arg_matches: &ArgMatches,
        authority_name: &str,
        wallet_manager: &mut Option<Arc<RemoteWalletManager>>,
    ) -> (Box<dyn Signer>, Pubkey) {
        // If there are `--multisig-signers` on the command line, allow `NullSigner`s to
        // be returned for multisig account addresses
        let config = SignerFromPathConfig {
            allow_null_signer: !self.multisigner_pubkeys.is_empty(),
        };
        let mut load_authority = move || {
            // fallback handled in default_signer() for backward compatibility
            if authority_name != "owner" {
                if let Some(keypair_path) = arg_matches.value_of(authority_name) {
                    return signer_from_path_with_config(
                        arg_matches,
                        keypair_path,
                        authority_name,
                        wallet_manager,
                        &config,
                    );
                }
            }

            self.default_signer(arg_matches, wallet_manager, &config)
        };

        let authority = load_authority().unwrap_or_else(|e| {
            eprintln!("error: {}", e);
            exit(1);
        });

        let authority_address = authority.pubkey();
        (authority, authority_address)
    }

    fn default_address(
        &self,
        matches: &ArgMatches,
        wallet_manager: &mut Option<Arc<RemoteWalletManager>>,
    ) -> Result<Pubkey, Box<dyn std::error::Error>> {
        // for backwards compatibility, check owner before cli config default
        if let Some(address) = pubkey_of_signer(matches, "owner", wallet_manager).unwrap() {
            return Ok(address);
        }

        match &self.default_keypair {
            #[cfg(test)]
            KeypairOrPath::Keypair(keypair) => Ok(keypair.pubkey()),
            KeypairOrPath::Path(path) => pubkey_from_path(matches, path, "default", wallet_manager),
        }
    }

    fn default_signer(
        &self,
        matches: &ArgMatches,
        wallet_manager: &mut Option<Arc<RemoteWalletManager>>,
        config: &SignerFromPathConfig,
    ) -> Result<Box<dyn Signer>, Box<dyn std::error::Error>> {
        // for backwards compatibility, check owner before cli config default
        if let Some(owner_path) = matches.value_of("owner") {
            return signer_from_path_with_config(
                matches,
                owner_path,
                "owner",
                wallet_manager,
                config,
            );
        }

        match &self.default_keypair {
            #[cfg(test)]
            KeypairOrPath::Keypair(keypair) => {
                let cloned = Keypair::from_bytes(&keypair.to_bytes()).unwrap();
                Ok(Box::new(cloned))
            }
            KeypairOrPath::Path(path) => {
                signer_from_path_with_config(matches, path, "default", wallet_manager, config)
            }
        }
    }

    pub(crate) async fn get_mint_info(
        &self,
        mint: &Pubkey,
        mint_decimals: Option<u8>,
    ) -> Result<MintInfo, Error> {
        if self.sign_only {
            Ok(MintInfo {
                program_id: self.program_id,
                address: *mint,
                decimals: mint_decimals.unwrap_or_default(),
            })
        } else {
            let account = self.rpc_client.get_account(mint).await?;
            self.check_owner(mint, &account.owner)?;
            let mint_account = StateWithExtensionsOwned::<Mint>::unpack(account.data)
                .map_err(|_| format!("Could not find mint account {}", mint))?;
            if let Some(decimals) = mint_decimals {
                if decimals != mint_account.base.decimals {
                    return Err(format!(
                        "Mint {:?} has decimals {}, not configured decimals {}",
                        mint, mint_account.base.decimals, decimals
                    )
                    .into());
                }
            }
            Ok(MintInfo {
                program_id: account.owner,
                address: *mint,
                decimals: mint_account.base.decimals,
            })
        }
    }

    pub(crate) fn check_owner(&self, account: &Pubkey, owner: &Pubkey) -> Result<(), Error> {
        if self.program_id != *owner {
            Err(format!(
                "Account {:?} is owned by {}, not configured program id {}",
                account, owner, self.program_id
            )
            .into())
        } else {
            Ok(())
        }
    }

    pub(crate) async fn check_account(
        &self,
        token_account: &Pubkey,
        mint_address: Option<Pubkey>,
    ) -> Result<Pubkey, Error> {
        if !self.sign_only {
            let account = self.rpc_client.get_account(token_account).await?;
            let source_account = StateWithExtensionsOwned::<Account>::unpack(account.data)
                .map_err(|_| format!("Could not find token account {}", token_account))?;
            let source_mint = source_account.base.mint;
            if let Some(mint) = mint_address {
                if source_mint != mint {
                    return Err(format!(
                        "Source {:?} does not contain {:?} tokens",
                        token_account, mint
                    )
                    .into());
                }
            }
            self.check_owner(token_account, &account.owner)?;
            Ok(source_mint)
        } else {
            Ok(mint_address.unwrap_or_default())
        }
    }
}
