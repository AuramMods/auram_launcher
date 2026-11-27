import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Button } from './ui/button';
import { Input } from './ui/input';

type Props = {
  installed?: string;
  pinned?: string;
  followLabel: string;
  disabled: boolean;
  onSelect: (version: string | null) => void;
};

export function PackVersionPicker({ installed, pinned, followLabel, disabled, onSelect }: Props) {
  const [versions, setVersions] = useState<string[]>([]);
  const [query, setQuery] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let current = true;
    invoke<string[]>('list_pack_versions')
      .then(tags => { if (current) setVersions(tags); })
      .catch(error => { if (current) setError(String(error)); })
      .finally(() => { if (current) setLoading(false); });
    return () => { current = false; };
  }, [attempt]);
  const visible = versions.filter(version => version.includes(query.trim().toLowerCase()));
  return <div className="grid gap-2">
    <Button variant="outline" className="justify-start" disabled={disabled||!pinned} onClick={() => onSelect(null)}>
      {followLabel}{!pinned && ' · Selected'}
    </Button>
    <Input aria-label="Find a pack version" placeholder="Find a pack version" value={query} onChange={event => setQuery(event.target.value)}/>
    {loading && <p className="text-xs text-muted-foreground" role="status">Loading pack versions…</p>}
    {error && <div className="grid gap-2">
      <p className="text-xs text-muted-foreground" role="alert">{error}</p>
      <Button variant="outline" size="sm" onClick={() => {setError('');setLoading(true);setAttempt(value => value + 1);}}>Retry</Button>
    </div>}
    {!loading && !error && !visible.length && <p className="text-xs text-muted-foreground">No matching versions.</p>}
    {!!visible.length && <ul className="max-h-56 overflow-y-auto grid gap-1" aria-label="Pack versions">
      {visible.map(version => <li key={version}>
        <Button variant={version===pinned?'secondary':'ghost'} className="w-full justify-between"
          disabled={disabled} onClick={() => onSelect(version)}>
          <span>Auram {version}</span>
          <span className="text-xs text-muted-foreground">
            {version===installed?'Installed':''}{version===installed&&version===pinned?' · ':''}{version===pinned?'Pinned':''}
          </span>
        </Button>
      </li>)}
    </ul>}
  </div>;
}
