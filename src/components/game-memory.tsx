import React from 'react';
import { TriangleAlert } from 'lucide-react';
import { Button } from './ui/button';
import { Card, CardContent, CardHeader, CardTitle } from './ui/card';
import { Label } from './ui/label';
import {
  describeGameMemory,
  heapNotches,
  heapOverheadGib,
  heapPosition,
  maximumHeapGib,
  minimumHeapGib,
  type AutomaticMemory,
  type MemoryPreferences,
} from '../lib/game-memory';

type Props = {
  automaticMemory: AutomaticMemory | null;
  preferences: MemoryPreferences | null;
  disabled: boolean;
  onSave: (preferences: MemoryPreferences) => Promise<boolean>;
};

export function GameMemoryCard({ automaticMemory, preferences, disabled, onSave }: Props) {
  const [draft, setDraft] = React.useState<MemoryPreferences | null>(null);
  const [saving, setSaving] = React.useState(false);
  const selection = draft ?? preferences;
  const memory = selection && automaticMemory
    ? describeGameMemory(selection, automaticMemory.maximumMib)
    : null;
  const locked = disabled || saving || !memory;
  const changed = draft !== null
    && (draft.automatic !== preferences?.automatic || draft.manualHeapGib !== preferences?.manualHeapGib);

  async function save() {
    if (!draft) return;
    setSaving(true);
    try {
      if (await onSave(draft)) setDraft(null);
    } finally {
      setSaving(false);
    }
  }

  return <Card>
    <CardHeader>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <CardTitle>Game memory</CardTitle>
        <Label htmlFor="automatic-game-memory" className="text-xs text-muted-foreground">
          <input
            id="automatic-game-memory"
            type="checkbox"
            className="game-memory-checkbox"
            checked={selection?.automatic ?? true}
            disabled={locked}
            onChange={event => {
              if (!selection || !memory) return;
              setDraft({
                automatic: event.target.checked,
                manualHeapGib: selection.manualHeapGib ?? memory.heapGib,
              });
            }}
          />
          Manage memory automatically
        </Label>
      </div>
    </CardHeader>
    <CardContent>
      {memory && selection && automaticMemory ? <>
        <div className="game-memory-value">{memory.heapGib} <span>GB max heap</span></div>
        <div className="game-memory-slider">
          <input
            type="range"
            aria-label="Maximum game heap"
            aria-valuetext={`${memory.heapGib} GB`}
            min={minimumHeapGib}
            max={maximumHeapGib}
            step={1}
            value={memory.heapGib}
            disabled={locked || selection.automatic}
            style={{ '--heap-position': `${heapPosition(memory.heapGib)}%` } as React.CSSProperties}
            onChange={event => setDraft({ ...selection, manualHeapGib: Number(event.target.value) })}
          />
          <div className="game-memory-scale" aria-hidden="true">
            <span className="game-memory-min">{minimumHeapGib} GB</span>
            {heapNotches.map(notch => <span className="game-memory-notch" key={notch}
              style={{ left: `${heapPosition(notch)}%` }}>{notch} GB</span>)}
            <span className="game-memory-max">{maximumHeapGib} GB</span>
          </div>
        </div>
        <p className="mt-4 text-xs text-muted-foreground">
          <span className="text-violet-200">{memory.profile} tuning</span> · {memory.initialGib} GB initial heap
        </p>
        {selection.automatic && <p className="mt-1 text-xs text-muted-foreground">
          {automaticMemory.systemMemoryMib !== null
            ? `${(automaticMemory.systemMemoryMib / 1024).toFixed(1)} GB of system memory detected.`
            : `System memory could not be detected. The ${memory.heapGib} GB preset is selected.`}
        </p>}
        <div className="mt-3" aria-live="polite">
          <p className="text-xs text-muted-foreground">
            Auram may use about <strong className="text-foreground">{memory.totalGib} GB total</strong>,
            including roughly {heapOverheadGib} GB of off-heap memory for Distant Horizons.
          </p>
          {memory.warnings.length > 0 && <ul className="game-memory-warnings">
            {memory.warnings.map(warning => <li key={warning}><TriangleAlert size={14} aria-hidden="true"/>{warning}</li>)}
          </ul>}
        </div>
        {changed && <div className="mt-4 flex flex-wrap items-center gap-3">
          <Button size="sm" disabled={locked} onClick={() => void save()}>{saving ? 'Saving…' : 'Save memory settings'}</Button>
          <span className="text-xs text-muted-foreground">Applies on the next game launch.</span>
        </div>}
      </> : <p className="text-xs text-muted-foreground">Detecting system memory…</p>}
    </CardContent>
  </Card>;
}
