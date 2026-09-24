import { LodestoneContext } from 'data/LodestoneContext';
import { useContext } from 'react';
import MyNavigate from './MyNavigate';
import { SAME_ORIGIN_CORE } from 'utils/util';

export default function RequireCore({
  children,
  redirect = '/first_setup',
}: {
  children: React.ReactNode;
  redirect?: string;
}) {
  const { coreList } = useContext(LodestoneContext);
  // With a same-origin core there is always exactly one core: the one serving
  // the dashboard. Nothing to pick, so never send anyone to core selection.
  if (SAME_ORIGIN_CORE) return <>{children}</>;
  return coreList.length > 0 ? <>{children}</> : <MyNavigate to={redirect} />;
}
