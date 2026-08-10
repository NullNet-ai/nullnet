import { BrowserRouter, Routes, Route } from 'react-router-dom';
import { StackProvider } from './StackContext';
import { AuthProvider } from './AuthContext';
import RequireAuth from './components/RequireAuth';
import Login from './pages/Login';
import Dashboard from './pages/Dashboard';
import Services from './pages/Services';
import Nodes from './pages/Nodes';
import Sessions from './pages/Sessions';
import Config from './pages/Config';
import Events from './pages/Events';
import Certificates from './pages/Certificates';
import Topology from './pages/Topology';
import Users from './pages/Users';
import DebugTopology from './pages/DebugTopology';

export default function App() {
  return (
    <AuthProvider>
      <StackProvider>
        <BrowserRouter>
          <Routes>
            <Route path="/login" element={<Login />} />
            {/* Not linked from the sidebar — dev tool, renders pasted/loaded
                JSON with no backend involved, so it doesn't need auth. */}
            <Route path="/debug/topology" element={<DebugTopology />} />
            <Route path="/" element={<RequireAuth><Dashboard /></RequireAuth>} />
            <Route path="/services" element={<RequireAuth><Services /></RequireAuth>} />
            <Route path="/nodes" element={<RequireAuth><Nodes /></RequireAuth>} />
            <Route path="/sessions" element={<RequireAuth><Sessions /></RequireAuth>} />
            <Route path="/config" element={<RequireAuth><Config /></RequireAuth>} />
            <Route path="/certificates" element={<RequireAuth><Certificates /></RequireAuth>} />
            <Route path="/events" element={<RequireAuth><Events /></RequireAuth>} />
            <Route path="/topology" element={<RequireAuth><Topology /></RequireAuth>} />
            <Route path="/users" element={<RequireAuth><Users /></RequireAuth>} />
          </Routes>
        </BrowserRouter>
      </StackProvider>
    </AuthProvider>
  );
}
