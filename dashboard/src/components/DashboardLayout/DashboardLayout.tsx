import TopNav from './TopNav';
import { useContext } from 'react';
import { useEventStream } from 'data/EventStream';
import { useCoreInfo } from 'data/SystemInfo';
import { useEffect, useState } from 'react';
import NotificationPanel from './NotificationPanel';
import { useUserInfo } from 'data/UserInfo';
import { BrowserLocationContext } from 'data/BrowserLocationContext';
import { Outlet } from 'react-router-dom';
import ConfirmDialog from 'components/Atoms/ConfirmDialog';
import { Popover } from '@headlessui/react';
import { LodestoneContext } from 'data/LodestoneContext';
import { major, minor, patch, valid, eq } from 'semver';
import { toast } from 'react-toastify';
import { useLocalStorage } from 'usehooks-ts';
import packageinfo from '../../../package.json';

export default function DashboardLayout() {
  const { data: userInfo } = useUserInfo();
  const { setPathname } = useContext(BrowserLocationContext);
  useEventStream();

  /* Start Core */
  const { coreConnectionStatus, core } = useContext(LodestoneContext);
  const [showSetupPrompt, setShowSetupPrompt] = useState(false);
  const { data: coreInfo, isLoading: coreInfoLoading } = useCoreInfo();
  const [showVersionMismatchModal, setShowVersionMismatchModal] =
    useState(false);
  const [showThankYouModal, setShowThankYouModal] =
    useState(false);
  const [showCoreErrorModal, setShowCoreErrorModal] = useState(false);
  const [latestVersion, setLatestVersion] = useLocalStorage('latestVersion', '0.0.0');
  const dashboardVersion = packageinfo.version;

  // open the error modal is coreConnectionStatus is error for more than 3 seconds
  useEffect(() => {
    setShowCoreErrorModal(coreConnectionStatus === 'error')
  }, [coreConnectionStatus]);

  useEffect(() => {
    if (latestVersion === coreInfo?.version || !coreInfo?.version)
      return;
    
    if ((coreInfo.version !== latestVersion || latestVersion === '0.0.0') 
        && coreInfo.version.includes("beta"))
      setShowThankYouModal(true);
     
    setLatestVersion(coreInfo.version);
    
  }, [coreInfo])


  const thankYouModal =  (
    <ConfirmDialog
      title={`Thanks for using Lodestone beta!`}
      type={'info'}
      isOpen={showThankYouModal}
      onClose={() => setShowThankYouModal(false)}
      closeButtonText={'Close'}
    >
      Thanks for using Lodestone beta! If you find any issues please let us 
      know on our Discord server or on Github.
    </ConfirmDialog>
  )

  const versionMismatchModal = !coreInfoLoading && (
    <ConfirmDialog
      title={`Update Required!`}
      type={'danger'}
      isOpen={showVersionMismatchModal}
      onClose={() => setShowVersionMismatchModal(false)}
      closeButtonText={'I understand, continue without updating'}
    >
      <div>
        <b>Core Version: </b>
        {coreInfo?.version}
        <br />
        <b>Dashboard Version: </b>
        {dashboardVersion}
      </div>
      <br />
      <p className="text-red-200">Your dashboard and core is incompatible!</p>
      This can cause unexpected behavior. Please update your core to the latest
      version. Visit{' '}
      <a
        href="https://github.com/Lodestone-Team/lodestone/wiki/Updating"
        className="text-blue-200"
      >
        the wiki
      </a>{' '}
      for more information.
    </ConfirmDialog>
  );

  useEffect(() => {
    if (coreInfo?.is_setup === false) {
      setShowSetupPrompt(true);
    }
  }, [coreInfo]);

  /* End Core */

  useEffect(() => {
    const clientVersion = coreInfoLoading ? undefined : coreInfo?.version;
    if (clientVersion === undefined) return;
    if (valid(clientVersion) && valid(dashboardVersion)) {
      if (eq(clientVersion, dashboardVersion)) return;
      if (major(clientVersion) !== major(dashboardVersion))
        setShowVersionMismatchModal(true);
      else if (minor(clientVersion) !== minor(dashboardVersion))
        // toast.warn(
        //   `There is a minor version mismatch! Core: ${clientVersion}, Dashboard: ${dashboardVersion}`,
        //   { toastId: 'minorVersionMismatch' }
        // );
        setShowVersionMismatchModal(true);
      else if (
        major(clientVersion) === 0 &&
        minor(clientVersion) === 4 &&
        patch(clientVersion) < 4
      )
        setShowVersionMismatchModal(true);
      else if (patch(clientVersion) !== patch(dashboardVersion))
        toast.warn(
          `Version mismatch! Is your core out of date? Core: ${clientVersion}, Dashboard: ${dashboardVersion}`,
          {
            toastId: 'patchVersionMismatch',
            autoClose: 10000,
            position: 'top-center',
          }
        );
    }
  }, [coreInfo?.version]);

  return (
    <>
      <ConfirmDialog
        isOpen={showSetupPrompt}
        title="Setup Required"
        type="info"
        z-index="20"
        confirmButtonText="Setup"
        onConfirm={() => {
          setPathname('/login/core/first_setup');
          setShowSetupPrompt(false);
        }}
        closeButtonText="Later"
        onClose={() => {
          setShowSetupPrompt(false);
        }}
      >
        {coreInfo?.core_name} is not setup yet. Please complete the setup
        process.
      </ConfirmDialog>
      <ConfirmDialog
        isOpen={showCoreErrorModal}
        title="Core Connection Error"
        type="info"
        z-index="20"
        confirmButtonText="Retry"
        onConfirm={() => {
          window.location.reload();
        }}
        closeButtonText="Close"
        onClose={() => {
          setShowCoreErrorModal(false);
        }}
      >
        There was an error connecting to {core.address}:{core.port}. Please
        refresh the page, or wait for the core to come back online.
      </ConfirmDialog>
      <div className="flex h-screen flex-col">
        <TopNav />
        <div className="flex min-h-0 w-full grow flex-row bg-gray-875">
          {versionMismatchModal}
          <Outlet />
        </div>
      </div>
      {thankYouModal}
    </>
  );
}
