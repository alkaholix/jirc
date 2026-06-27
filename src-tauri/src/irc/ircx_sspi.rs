//! NTLM client token engine — **vendored from `ircx-sspi`**
//! (github.com/realJoshByrnes/ircx-sspi, `src/ntlm.rs`: the MSN Chat NTLM
//! Security Provider). That crate drives the cross-platform `sspi` crate's NTLM
//! state machine; this is the *client* leg of its `initialize_security_context`
//! (Type 1 negotiate → Type 3 authenticate), lifted out on its own and pointed
//! at the connection's **real** domain/user/password.
//!
//! ## Why this is vendored, not a dependency on the `ircx-sspi` crate
//!
//! (Assessed 2026-06-24 — revisit if upstream changes.) Three blockers make the
//! crate unusable as a dependency here, so we depend on `sspi` directly and keep
//! only the client leg:
//!   1. Its sole client path (`NtlmSecurityProvider::initialize_security_context`)
//!      hardcodes `TestUser`/`password`; there is no public way to pass real
//!      credentials, so it cannot actually log a user in. [`NtlmSession::new`]
//!      below takes the real identity instead.
//!   2. It drags in `eframe` (the egui/winit GUI stack) plus aes-gcm/md4/
//!      getrandom and is edition 2024 — far too heavy for pure local crypto. We
//!      keep only the `sspi` state machine and drop the Windows-SSP C-ABI
//!      scaffolding (`SecBuffer`/`CredHandle`/`CtxtHandle`/`SecurityProvider` +
//!      session map) the DLL build needs and a token engine does not.
//!   3. It ships **no LICENSE**, so an MIT project cannot take a code/git
//!      dependency on it; confirm redistribution terms with the author before
//!      shipping. If a license *and* a real-credential client API land upstream,
//!      this file can shrink to a thin call into the crate.
//!
//! The call sequence here is the canonical `sspi` client NTLM pattern (as the
//! `sspi` docs show), structured to mirror ircx-sspi's `NtlmSession`.

use sspi::{
    AuthIdentity, BufferType, ClientRequestFlags, CredentialUse, DataRepresentation, Ntlm,
    SecurityBuffer, Sspi, SspiImpl, Username,
};

/// An active client-side NTLM session, holding the `sspi::Ntlm` state machine
/// and its credentials handle across the two-leg handshake. Mirrors
/// ircx-sspi's `NtlmSession` (client fields only).
pub struct NtlmSession {
    ntlm: Ntlm,
    creds: <Ntlm as SspiImpl>::CredentialsHandle,
}

impl NtlmSession {
    /// Acquires an outbound NTLM credentials handle for the given identity.
    /// (Upstream's client step 0 — the identity is real here, not hardcoded.)
    pub fn new(domain: &str, user: &str, password: &str) -> Result<NtlmSession, String> {
        let mut ntlm = Ntlm::new();
        let username = Username::new(user, (!domain.is_empty()).then_some(domain))
            .map_err(|e| format!("NTLM username: {e}"))?;
        let identity = AuthIdentity {
            username,
            password: password.to_string().into(),
        };
        let acquired = ntlm
            .acquire_credentials_handle()
            .with_credential_use(CredentialUse::Outbound)
            .with_auth_data(&identity)
            .execute(&mut ntlm)
            .map_err(|e| format!("NTLM acquire credentials: {e}"))?;
        Ok(NtlmSession {
            ntlm,
            creds: acquired.credentials_handle,
        })
    }

    /// Client step 1: produce the Type 1 (negotiate) token that opens the handshake.
    pub fn negotiate(&mut self) -> Result<Vec<u8>, String> {
        let mut output = vec![SecurityBuffer::new(Vec::new(), BufferType::Token)];
        let mut builder = self
            .ntlm
            .initialize_security_context()
            .with_credentials_handle(&mut self.creds)
            .with_context_requirements(ClientRequestFlags::empty())
            .with_target_data_representation(DataRepresentation::Native)
            .with_output(&mut output);
        self.ntlm
            .initialize_security_context_impl(&mut builder)
            .map_err(|e| format!("NTLM negotiate: {e}"))?
            .resolve_to_result()
            .map_err(|e| format!("NTLM negotiate: {e}"))?;
        Ok(output.swap_remove(0).buffer)
    }

    /// Client step 2: produce the Type 3 (authenticate) token from the server's
    /// Type 2 challenge.
    pub fn authenticate(&mut self, challenge: &[u8]) -> Result<Vec<u8>, String> {
        let mut input = vec![SecurityBuffer::new(challenge.to_vec(), BufferType::Token)];
        let mut output = vec![SecurityBuffer::new(Vec::new(), BufferType::Token)];
        let mut builder = self
            .ntlm
            .initialize_security_context()
            .with_credentials_handle(&mut self.creds)
            .with_context_requirements(ClientRequestFlags::empty())
            .with_target_data_representation(DataRepresentation::Native)
            .with_input(&mut input)
            .with_output(&mut output);
        self.ntlm
            .initialize_security_context_impl(&mut builder)
            .map_err(|e| format!("NTLM authenticate: {e}"))?
            .resolve_to_result()
            .map_err(|e| format!("NTLM authenticate: {e}"))?;
        Ok(output.swap_remove(0).buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntlm_state_is_send() {
        // `handshake` holds an `NtlmSession` across `.await`, so its sspi state
        // must be `Send` to run inside the multi-threaded tokio task
        // (see connection::run_once).
        fn assert_send<T: Send>() {}
        assert_send::<Ntlm>();
        assert_send::<<Ntlm as SspiImpl>::CredentialsHandle>();
    }
}
