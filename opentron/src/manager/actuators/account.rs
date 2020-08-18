//! Account related builtin contracts.

use std::convert::TryFrom;

use ::keys::Address;
use proto2::chain::transaction::Result as TransactionResult;
use proto2::chain::ContractType;
use proto2::common::{permission::PermissionType, Permission};
use proto2::contract as contract_pb;
use proto2::state::{Account, ActivePermission, OwnerPermission, PermissionKey};
use state::keys;

use super::super::executor::TransactionContext;
use super::super::Manager;
use super::BuiltinContractExecutorExt;

// Set account's name.
impl BuiltinContractExecutorExt for contract_pb::AccountUpdateContract {
    fn validate(&self, manager: &Manager, _ctx: &mut TransactionContext) -> Result<(), String> {
        let state_db = &manager.state_db;

        // validAccountName
        if self.account_name.as_bytes().len() > 200 {
            return Err("invalid account name".into());
        }

        let owner_address = Address::try_from(&self.owner_address).map_err(|_| "invalid owner_address")?;
        let maybe_acct = state_db
            .get(&keys::Account(owner_address))
            .map_err(|_| "db query error")?;
        if maybe_acct.is_none() {
            return Err("account not exists".into());
        }
        let acct = maybe_acct.unwrap();

        let allow_update_account_name = state_db.must_get(&keys::ChainParameter::AllowUpdateAccountName) != 0;
        if !acct.name.is_empty() && !allow_update_account_name {
            return Err("account name already exists".into());
        }

        if !allow_update_account_name && find_account_by_name(manager, &self.account_name).is_some() {
            return Err("the same account name already exists".into());
        }

        Ok(())
    }

    fn execute(&self, manager: &mut Manager, _ctx: &mut TransactionContext) -> Result<TransactionResult, String> {
        let owner_address = Address::try_from(&self.owner_address).unwrap();
        let mut owner_acct = manager.state_db.must_get(&keys::Account(owner_address));

        owner_acct.name = self.account_name.clone();

        manager
            .state_db
            .put_key(keys::Account(owner_address), owner_acct)
            .map_err(|e| e.to_string())?;
        Ok(TransactionResult::success())
    }
}

// Update account's permission for multisig or transfering ownership.
impl BuiltinContractExecutorExt for contract_pb::AccountPermissionUpdateContract {
    fn validate(&self, manager: &Manager, ctx: &mut TransactionContext) -> Result<(), String> {
        let state_db = &manager.state_db;

        if state_db.must_get(&keys::ChainParameter::AllowMultisig) == 0 {
            return Err("multisig is disabled on chain".into());
        }

        let owner_address = Address::try_from(&self.owner_address).map_err(|_| "invalid owner_address")?;
        let maybe_acct = state_db
            .get(&keys::Account(owner_address))
            .map_err(|_| "db query error")?;
        if maybe_acct.is_none() {
            return Err("account not exists".into());
        }
        let acct = maybe_acct.unwrap();

        if self.owner.is_none() {
            return Err("missing owner permission".into());
        }

        let is_witness = state_db
            .get(&keys::Witness(owner_address))
            .map_err(|_| "error while querying db")?
            .is_some();
        if is_witness {
            if let Some(wit_perm) = self.witness.as_ref() {
                check_permission(wit_perm, PermissionType::Witness)?;
            } else {
                return Err("missing witness permission".into());
            }
        } else if self.witness.is_some() {
            return Err("account is not a witness".into());
        }

        if self.actives.is_empty() {
            return Err("missing active permissions".into());
        }
        if self.actives.len() > constants::MAX_NUM_OF_ACTIVE_PERMISSIONS {
            return Err("too many active permissions".into());
        }

        check_permission(self.owner.as_ref().unwrap(), PermissionType::Owner)?;

        for active in &self.actives {
            check_permission(active, PermissionType::Active)?;
        }

        let fee = self.fee(manager);
        if acct.balance < fee {
            return Err("insufficient balance to set account permission".into());
        }
        ctx.contract_fee = fee;

        Ok(())
    }

    fn execute(&self, manager: &mut Manager, ctx: &mut TransactionContext) -> Result<TransactionResult, String> {
        let owner_address = Address::try_from(&self.owner_address).unwrap();
        let mut owner_acct = manager.state_db.must_get(&keys::Account(owner_address));

        // updatePermissions
        if let Some(owner_perm) = self.owner.as_ref() {
            owner_acct.owner_permission = Some(OwnerPermission {
                threshold: owner_perm.threshold,
                keys: owner_perm
                    .keys
                    .iter()
                    .map(|key| PermissionKey {
                        address: key.address.clone(),
                        weight: key.weight,
                    })
                    .collect(),
            });
        }

        owner_acct.active_permissions = self
            .actives
            .iter()
            .map(|perm| ActivePermission {
                threshold: perm.threshold,
                keys: perm
                    .keys
                    .iter()
                    .map(|key| PermissionKey {
                        address: key.address.clone(),
                        weight: key.weight,
                    })
                    .collect(),
                operations: perm.operations.clone(),
                permission_name: perm.name.clone(),
            })
            .collect();

        if let Some(wit_perm) = self.witness.as_ref() {
            let mut wit = manager.state_db.must_get(&keys::Witness(owner_address));
            wit.signature_key = wit_perm.keys[0].address.clone();

            manager
                .state_db
                .put_key(keys::Witness(owner_address), wit)
                .map_err(|e| e.to_string())?;
        }

        if ctx.contract_fee != 0 {
            owner_acct.adjust_balance(-ctx.contract_fee).unwrap();
            manager.add_to_blackhole(ctx.contract_fee).unwrap();
        }
        manager
            .state_db
            .put_key(keys::Account(owner_address), owner_acct)
            .map_err(|e| e.to_string())?;

        Ok(TransactionResult::success())
    }

    fn fee(&self, manager: &Manager) -> i64 {
        manager
            .state_db
            .must_get(&keys::ChainParameter::AccountPermissionUpdateFee)
    }
}

// TODO: impl index
/// Find an account in state-db by its name.
fn find_account_by_name(manager: &Manager, acct_name: &str) -> Option<Account> {
    let mut found: Option<Account> = None;
    {
        let found = &mut found;
        manager.state_db.for_each(move |_key: &keys::Account, value: &Account| {
            if value.name == acct_name {
                *found = Some(value.clone());
            }
        });
    }
    found
}

/// Check permission pb definition.
fn check_permission(perm: &Permission, perm_type: PermissionType) -> Result<(), String> {
    if perm.keys.len() > constants::MAX_NUM_OF_KEYS_IN_PERMISSION {
        return Err(format!(
            "number of keys in permission should not be greater than {}",
            constants::MAX_NUM_OF_KEYS_IN_PERMISSION
        ));
    }
    if perm.keys.is_empty() {
        return Err("no permission key provided".into());
    }

    if perm.threshold <= 0 {
        return Err("permission threshold should be greater than 0".into());
    }
    if perm.name.len() > 32 {
        return Err("permission name is too long".into());
    }
    if perm.parent_id != 0 {
        return Err("parent_id must be 0(owner)".into());
    }

    let mut weight_sum = 0_i64;
    let mut addrs: Vec<&[u8]> = Vec::with_capacity(perm.keys.len());
    for key in &perm.keys {
        if Address::try_from(&key.address).is_err() {
            return Err("invalid key address".into());
        }
        if key.weight <= 0 {
            return Err("weight of key should be greater than 0".into());
        }
        weight_sum = weight_sum.checked_add(key.weight).ok_or("math overflow")?;

        if addrs.contains(&&*key.address) {
            return Err("duplicated address in keys".into());
        } else {
            addrs.push(&*key.address);
        }
    }

    if weight_sum < perm.threshold {
        return Err("sum of all weights should be greater than threshold".into());
    }

    match perm_type {
        PermissionType::Owner | PermissionType::Witness => {
            if !perm.operations.is_empty() {
                return Err("no operations vec needed".into());
            }
        }
        PermissionType::Active => {
            if perm.operations.is_empty() || perm.operations.len() != 32 {
                return Err("operations vec length must be 32".into());
            }
            // NOTE: The check logic is buggy in java-tron.
            for type_code in 0..256 {
                let mask = (perm.operations[type_code / 8] >> (type_code % 8)) & 1;
                if mask != 0 && ContractType::from_i32(type_code as i32).is_none() {
                    return Err(format!("operation of {} is undefined", type_code));
                }
            }
        }
    }

    Ok(())
}
