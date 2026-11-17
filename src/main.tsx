import React from 'react';
import { createRoot } from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import './style.css';
function App() {
  const [message, setMessage] = React.useState('Connecting to launcher…');
  React.useEffect(() => { invoke<string>('ping').then(setMessage).catch(error => setMessage(String(error))); }, []);
  return <main><span className="eyebrow">AURAM LAUNCHER</span><h1>Your next<br/><em>adventure.</em></h1><p>{message}</p></main>;
}
createRoot(document.getElementById('root')!).render(<React.StrictMode><App/></React.StrictMode>);
