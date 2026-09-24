import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import {
  CoreConnectionInfo,
  CoreConnectionStatus,
  LodestoneContext,
} from 'data/LodestoneContext';
import {
  NotificationContext,
  useNotificationReducer,
  useOngoingNotificationReducer,
} from 'data/NotificationContext';
import React, { useContext, useLayoutEffect, useState } from 'react';
import { Routes, Route, Navigate } from 'react-router-dom';
import { useLocalStorage } from 'usehooks-ts';
import { CORE, errorToString } from 'utils/util';
import Dashboard from 'pages/dashboard';
import Home from 'pages/home';
import axios from 'axios';
import jwt from 'jsonwebtoken';
import DashboardLayout from 'components/DashboardLayout';
import SettingsPage from 'pages/settings';
import UserSelectExisting from 'pages/login/UserSelectExisting';
import UserLogin from 'pages/login/UserLogin';
import CoreSetupNew from 'pages/login/CoreSetupNew';
import CoreConfigNew from 'pages/login/CoreConfigNew';
import LoginLayout from 'components/LoginLayout';
import { BrowserLocationContext } from 'data/BrowserLocationContext';
import NotFound from 'pages/notfound';
import RequireToken from 'utils/router/RequireToken';
import { InstanceViewLayout } from 'components/DashboardLayout/InstanceViewLayout';
import { SettingsLayout } from 'components/DashboardLayout/SettingsLayout';
import { toast } from 'react-toastify';
import RequireSetup from 'utils/router/RequireSetup';
import InstanceTabs from 'pages/InstanceTabs/InstanceTabs';
import ProfilePage from 'pages/settings/profile';
import CoreSettings from 'pages/settings/CoreSettings';
import UserSettings from 'pages/settings/UserSettings';


const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: Infinity,
      refetchOnWindowFocus: false,
    },
  },
});

const InstanceTabList = [
  'overview',
  'settings',
  'console',
  'files',
  'macros',
  'logs',
  'playitgg',
  'tunnels',
];

export const CoreSettingsTabList = [
    {
    title: 'General',
      path: 'general',
      content: <CoreSettings />,
    },
    {
      title: 'Users',
      path: 'users',
      content: <UserSettings />,
    },
];

export const AccountSettingsTabList = [
    {
    title: 'Profile',
      path: 'profile',
      content: <ProfilePage />,
    },
];

export default function App() {
  const { location } = useContext(BrowserLocationContext);

  /* Start Core */
  // Always the core on this origin (see CORE in utils/util): REST calls go to
  // /api/v1 on the same host, websockets to the same host and port.
  const core: CoreConnectionInfo = CORE;
  const { address, port, apiVersion } = core;
  const socket = `${address}:${port}`;
  const [coreConnectionStatus, setCoreConnectionStatus] =
    useState<CoreConnectionStatus>('loading');
  useLayoutEffect(() => {
    axios.defaults.baseURL = `/api/${apiVersion}`;
    setCoreConnectionStatus('loading');
  }, [apiVersion]);
  /* End Core */

  /* Start Token */
  const [tokens, setTokens] = useLocalStorage<Record<string, string>>(
    'tokens',
    {}
  ); //TODO: clear all outdated tokens
  const [uid, setUid] = useState('');
  const token = tokens[socket] ?? '';
  const setToken = (token: string, coreSocket: string) => {
    setTokens({ ...tokens, [coreSocket]: token });
  };
  useLayoutEffect(() => {
    if (!token) {
      delete axios.defaults.headers.common['Authorization'];
      dispatch({
        type: 'clear',
      });
      // TODO: clear ongoing notifications as well
    } else {
      try {
        const decoded = jwt.decode(token, { complete: true });
        if (!decoded) throw new Error('Invalid token');
        const payload = decoded.payload;
        if (typeof payload === 'undefined') throw new Error('Invalid token');
        if (typeof payload === 'string') throw new Error('Invalid token');
        const exp = payload.exp;
        const uid = payload.uid;

        if (typeof exp === 'undefined') throw new Error('Invalid exp in token');
        if (typeof uid !== 'string') throw new Error('Invalid uid in token');
        if (Date.now() >= exp * 1000) throw new Error('Token expired');
        setUid(uid);
        axios.defaults.headers.common['Authorization'] = `Bearer ${token}`;
      } catch (e) {
        const message = errorToString(e);
        toast.error(message);
        setToken('', socket);
        setUid('');
        delete axios.defaults.headers.common['Authorization'];
      }
    }
    queryClient.invalidateQueries();
    queryClient.clear();

    // only token in the dependency list
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [token]);
  /* End Token */

  /* Start Notifications */
  const { notifications, dispatch } = useNotificationReducer();
  const { ongoingNotifications, ongoingDispatch } =
    useOngoingNotificationReducer();
  /* End Notifications */

  return (
    <QueryClientProvider client={queryClient}>
      <LodestoneContext.Provider
        value={{
          core,
          coreConnectionStatus,
          setCoreConnectionStatus,
          token,
          uid,
          setToken,
          tokens,
        }}
      >
        <NotificationContext.Provider
          value={{
            notifications,
            dispatch,
            ongoingNotifications,
            ongoingDispatch,
          }}
        >
          <Routes>
            <Route element={<LoginLayout />}>
              <Route
                path="/login/core/first_setup"
                element={<CoreSetupNew />}
              />
              <Route
                path="/login/core/first_config"
                element={<CoreConfigNew />}
              />
              <Route
                path="/login/user/select"
                element={
                  <RequireSetup>
                    <UserSelectExisting />
                  </RequireSetup>
                }
              />
              <Route
                path="/login/user"
                element={
                  <RequireSetup>
                    <UserLogin />
                  </RequireSetup>
                }
              />
            </Route>
            <Route
              element={
                <RequireToken>
                  <DashboardLayout />
                </RequireToken>
              }
            >
              <Route element={<InstanceViewLayout />}>
                {/* <Route path="/dashboard" element={<Dashboard />} /> */}
                {InstanceTabList.map((path, i) => (
                  <Route
                    path={`/dashboard/${path}`}
                    element={<InstanceTabs />}
                    key={i}
                  />
                ))}
                <Route path="/" element={<Home />} />
              </Route>
              <Route element={<SettingsLayout />}>
                {CoreSettingsTabList.map((tab, i) => (
                  <Route
                    path={`/settings/${tab.path}`}
                    element={tab.content}
                    key={i}
                  />
                ))}
                {AccountSettingsTabList.map((tab, i) => (
                  <Route
                    path={`/settings/${tab.path}`}
                    element={tab.content}
                    key={i}
                  />
                ))}
              </Route>
            </Route>
            <Route path="*" element={<NotFound />} />
          </Routes>
        </NotificationContext.Provider>
      </LodestoneContext.Provider>
    </QueryClientProvider>
  );
}
