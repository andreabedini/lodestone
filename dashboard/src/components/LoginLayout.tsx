import { Outlet } from 'react-router-dom';
import { asset } from 'utils/util';

const LoginLayout = () => {
  return (
    <div
      className="flex h-screen flex-col justify-center p-16 lg:p-32"
      style={{
        background: `url('${asset('/login_background.svg')}')`,
        backgroundSize: 'cover',
        backgroundPosition: 'center',
      }}
    >
      <Outlet />
    </div>
  );
};

export default LoginLayout;
