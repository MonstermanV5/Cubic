use serde::{Deserialize, Serialize};
#[cfg(windows)]
use zeroize::Zeroizing;

use crate::{AuthBackend, AuthError, MinecraftProfile, SecretString};

#[cfg(windows)]
const SERVICE: &str = "Cubic Minecraft Authentication";
#[cfg(windows)]
const XAL_DEVICE_ACCOUNT: &str = "xal-interop-device";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredAccount {
    #[serde(default)]
    pub backend: AuthBackend,
    pub refresh_token: SecretString,
    pub profile: MinecraftProfile,
}

/// Persisted only through a platform secure store. Both fields are sensitive.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct XalDeviceCredential {
    pub device_id: SecretString,
    pub private_key: SecretString,
}

pub trait CredentialStore: Send + Sync {
    fn load_account(&self, backend: AuthBackend) -> Result<Option<StoredAccount>, AuthError>;
    fn save_account(&self, backend: AuthBackend, account: &StoredAccount) -> Result<(), AuthError>;
    fn delete_account(&self, backend: AuthBackend) -> Result<(), AuthError>;
    fn load_xal_device(&self) -> Result<Option<XalDeviceCredential>, AuthError>;
    fn save_xal_device(&self, device: &XalDeviceCredential) -> Result<(), AuthError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemCredentialStore;

#[cfg(windows)]
fn load_record<T: for<'de> Deserialize<'de>>(account: &str) -> Result<Option<T>, AuthError> {
    let entry =
        keyring::Entry::new(SERVICE, account).map_err(|_| AuthError::SecureStoreUnavailable)?;
    match entry.get_password() {
        Ok(value) => serde_json::from_str(&Zeroizing::new(value))
            .map(Some)
            .map_err(|_| AuthError::CorruptStoredCredential),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err(AuthError::SecureStoreUnavailable),
    }
}

#[cfg(windows)]
fn save_record<T: Serialize>(account: &str, value: &T) -> Result<(), AuthError> {
    let value = Zeroizing::new(
        serde_json::to_string(value).map_err(|_| AuthError::CorruptStoredCredential)?,
    );
    keyring::Entry::new(SERVICE, account)
        .and_then(|entry| entry.set_password(&value))
        .map_err(|_| AuthError::SecureStoreUnavailable)
}

#[cfg(windows)]
fn delete_record(account: &str) -> Result<(), AuthError> {
    let entry =
        keyring::Entry::new(SERVICE, account).map_err(|_| AuthError::SecureStoreUnavailable)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err(AuthError::SecureStoreUnavailable),
    }
}

#[cfg(windows)]
impl CredentialStore for SystemCredentialStore {
    fn load_account(&self, backend: AuthBackend) -> Result<Option<StoredAccount>, AuthError> {
        let account = load_record::<StoredAccount>(backend.credential_account())?;
        if account
            .as_ref()
            .is_some_and(|value| value.backend != backend)
        {
            return Err(AuthError::BackendMismatch);
        }
        Ok(account)
    }

    fn save_account(&self, backend: AuthBackend, account: &StoredAccount) -> Result<(), AuthError> {
        if account.backend != backend {
            return Err(AuthError::BackendMismatch);
        }
        save_record(backend.credential_account(), account)
    }

    fn delete_account(&self, backend: AuthBackend) -> Result<(), AuthError> {
        delete_record(backend.credential_account())
    }

    fn load_xal_device(&self) -> Result<Option<XalDeviceCredential>, AuthError> {
        load_record(XAL_DEVICE_ACCOUNT)
    }

    fn save_xal_device(&self, device: &XalDeviceCredential) -> Result<(), AuthError> {
        save_record(XAL_DEVICE_ACCOUNT, device)
    }
}

#[cfg(not(windows))]
impl CredentialStore for SystemCredentialStore {
    fn load_account(&self, _backend: AuthBackend) -> Result<Option<StoredAccount>, AuthError> {
        Err(AuthError::SecureStoreUnsupported)
    }

    fn save_account(
        &self,
        _backend: AuthBackend,
        _account: &StoredAccount,
    ) -> Result<(), AuthError> {
        Err(AuthError::SecureStoreUnsupported)
    }

    fn delete_account(&self, _backend: AuthBackend) -> Result<(), AuthError> {
        Err(AuthError::SecureStoreUnsupported)
    }

    fn load_xal_device(&self) -> Result<Option<XalDeviceCredential>, AuthError> {
        Err(AuthError::SecureStoreUnsupported)
    }

    fn save_xal_device(&self, _device: &XalDeviceCredential) -> Result<(), AuthError> {
        Err(AuthError::SecureStoreUnsupported)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, str::FromStr, sync::Mutex};

    use super::{CredentialStore, StoredAccount, XalDeviceCredential};
    use crate::{AuthBackend, AuthError, MinecraftProfile, MinecraftProfileId, SecretString};

    #[derive(Default)]
    struct MemoryStore {
        accounts: Mutex<HashMap<AuthBackend, StoredAccount>>,
        device: Mutex<Option<XalDeviceCredential>>,
    }

    impl CredentialStore for MemoryStore {
        fn load_account(&self, backend: AuthBackend) -> Result<Option<StoredAccount>, AuthError> {
            self.accounts
                .lock()
                .map_err(|_| AuthError::SecureStoreUnavailable)
                .map(|accounts| accounts.get(&backend).cloned())
        }

        fn save_account(
            &self,
            backend: AuthBackend,
            account: &StoredAccount,
        ) -> Result<(), AuthError> {
            if backend != account.backend {
                return Err(AuthError::BackendMismatch);
            }
            self.accounts
                .lock()
                .map_err(|_| AuthError::SecureStoreUnavailable)?
                .insert(backend, account.clone());
            Ok(())
        }

        fn delete_account(&self, backend: AuthBackend) -> Result<(), AuthError> {
            self.accounts
                .lock()
                .map_err(|_| AuthError::SecureStoreUnavailable)?
                .remove(&backend);
            Ok(())
        }

        fn load_xal_device(&self) -> Result<Option<XalDeviceCredential>, AuthError> {
            self.device
                .lock()
                .map_err(|_| AuthError::SecureStoreUnavailable)
                .map(|device| device.clone())
        }

        fn save_xal_device(&self, device: &XalDeviceCredential) -> Result<(), AuthError> {
            *self
                .device
                .lock()
                .map_err(|_| AuthError::SecureStoreUnavailable)? = Some(device.clone());
            Ok(())
        }
    }

    fn account(backend: AuthBackend, token: &str) -> StoredAccount {
        StoredAccount {
            backend,
            refresh_token: SecretString::new(token),
            profile: MinecraftProfile {
                id: MinecraftProfileId::from_str("0123456789abcdef0123456789abcdef").unwrap(),
                name: "CubicTest".into(),
            },
        }
    }

    #[test]
    fn secure_store_separates_backends_and_targeted_logout() {
        let store = MemoryStore::default();
        store
            .save_account(
                AuthBackend::CubicEntra,
                &account(AuthBackend::CubicEntra, "fake-entra"),
            )
            .unwrap();
        store
            .save_account(
                AuthBackend::XalInterop,
                &account(AuthBackend::XalInterop, "fake-xal"),
            )
            .unwrap();
        store.delete_account(AuthBackend::XalInterop).unwrap();
        assert!(
            store
                .load_account(AuthBackend::XalInterop)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .load_account(AuthBackend::CubicEntra)
                .unwrap()
                .unwrap()
                .refresh_token
                .expose(),
            "fake-entra"
        );
    }

    #[test]
    fn device_record_is_separate_from_accounts() {
        let store = MemoryStore::default();
        let device = XalDeviceCredential {
            device_id: SecretString::new("fake-device"),
            private_key: SecretString::new("fake-private-key"),
        };
        store.save_xal_device(&device).unwrap();
        store.delete_account(AuthBackend::XalInterop).unwrap();
        assert_eq!(
            store.load_xal_device().unwrap().unwrap().device_id.expose(),
            "fake-device"
        );
    }
}
