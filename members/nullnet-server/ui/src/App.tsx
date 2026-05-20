import { BrowserRouter, Routes, Route } from 'react-router-dom';
import { StackProvider } from './StackContext';
import Dashboard from './pages/Dashboard';
import Services from './pages/Services';
import Nodes from './pages/Nodes';
import Sessions from './pages/Sessions';
import Pool from './pages/Pool';
import Config from './pages/Config';

export default function App() {
  return (
    <StackProvider>
      <BrowserRouter>
        <Routes>
          <Route path="/" element={<Dashboard />} />
          <Route path="/services" element={<Services />} />
          <Route path="/nodes" element={<Nodes />} />
          <Route path="/sessions" element={<Sessions />} />
          <Route path="/pool" element={<Pool />} />
          <Route path="/config" element={<Config />} />
        </Routes>
      </BrowserRouter>
    </StackProvider>
  );
}
