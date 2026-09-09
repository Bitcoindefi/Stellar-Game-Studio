/**
 * Contract Signer interface for Stellar SDK bindings
 * Compatible with both Freighter wallet and dev wallets
 */
import type { SignTransaction, SignAuthEntry } from '@stellar/stellar-sdk/contract';

export interface ContractSigner {
  signTransaction: SignTransaction;
  signAuthEntry: SignAuthEntry;
}

