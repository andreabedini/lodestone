import { createContext } from 'react';
import { CORE } from 'utils/util';

export type CoreConnectionStatus = 'loading' | 'error' | 'degraded' | 'success';

interface LodestoneContext {
  core: CoreConnectionInfo;
  coreConnectionStatus: CoreConnectionStatus;
  setCoreConnectionStatus: (status: CoreConnectionStatus) => void;
  /** The JWT token string, where no token is an empty string */
  token: string;
  uid: string;
  /** Sets the JWT token in state and localStorage, where no token is an empty string */
  setToken: (token: string, coreSocket: string) => void;
  /** All the tokens, a record from CoreSocket to token */
  tokens: Record<string, string>;
}

export interface CoreConnectionInfo {
  address: string;
  port: string;
  protocol: string;
  apiVersion: string;
}

export const LodestoneContext = createContext<LodestoneContext>({
  core: CORE,
  coreConnectionStatus: 'loading',
  setCoreConnectionStatus: () => {
    console.error('setCoreConnectionStatus not implemented');
  },
  token: '',
  uid: '',
  setToken: () => {
    console.error('setToken not implemented');
  },
  tokens: {},
});
