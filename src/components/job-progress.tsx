import { Check, CircleAlert, LoaderCircle, X } from 'lucide-react';
import { Button } from './ui/button';
import { Progress } from './ui/progress';
import { formatBytes, formatPercent, type JobProgress } from '../lib/job-progress';

export function JobProgressPanel({progress,onCancel}:{progress:JobProgress|null;onCancel:()=>void}) {
  const phase=progress?.phases[progress.activePhase];
  const batch=phase?.batch;
  const settled=progress?.phases.filter(p=>p.state==='complete'||p.state==='skipped').length??0;
  const cancelling=progress?.state==='cancelling';
  const stopped=progress?.state==='failed'||progress?.state==='cancelled';
  return <section className="job-progress-panel" aria-label="Operation progress">
    <div className="job-overview"><div className="job-title"><span className="job-status-icon">{stopped?<CircleAlert size={17}/>:progress?.state==='complete'?<Check size={17}/>:<LoaderCircle size={17} className="animate-spin"/>}</span><div><strong>{progress?.title??'Getting ready'}</strong><p role="status" aria-live="polite">{cancelling?'Stopping safely…':stopped?progress?.state==='failed'?'Operation failed':'Operation cancelled':phase?`Step ${progress!.activePhase+1} of ${progress!.phases.length} · ${phase.label}`:'Starting operation…'}</p></div></div><div className="job-controls"><span className="job-step-count">{progress?`${settled} / ${progress.phases.length} steps`:''}</span><Button size="sm" variant="outline" onClick={onCancel} disabled={cancelling||stopped||progress?.state==='complete'}><X/>{cancelling?'Stopping':'Cancel'}</Button></div></div>
    {!!progress?.phases.length&&<ol className="job-phases" style={{gridTemplateColumns:`repeat(${progress.phases.length}, minmax(0, 1fr))`}} aria-label="Installation steps">
      {progress.phases.map((step,index)=><li key={step.id} className={`job-phase phase-${step.state}`} aria-current={step.state==='running'?'step':undefined} title={`${step.label}: ${step.state}`}>
        <div className="job-phase-track" role="progressbar" aria-label={step.label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={step.percent??undefined} aria-valuetext={step.state==='running'?(step.percent===null?'Working':`${formatPercent(step.percent)} complete`):step.state}>
          <span className={`job-phase-fill ${step.state==='running'&&step.percent===null?'phase-working':''}`} style={{width:`${step.percent??0}%`}}/>
        </div>
        <span className="job-phase-label" aria-hidden="true">
          <span className="job-phase-marker">{step.state==='complete'?<Check size={10}/>:step.state==='skipped'?'—':index+1}</span>
          <span className="job-phase-name">{step.label}</span>
        </span>
      </li>)}
    </ol>}
    <div className="job-batch"><div className="job-batch-main"><span>{batch?batch.label:phase?.detail||phase?.label||'Preparing work'}</span>{batch&&<strong>{formatPercent(batch.percent)} <small>{batch.basis==='bytes'?'of bytes':'of files'} verified</small></strong>}{!batch&&phase?.percent!==null&&phase?.percent!==undefined&&<strong>{formatPercent(phase.percent)} <small>complete</small></strong>}</div>{(batch||phase?.percent!==null&&phase?.percent!==undefined)&&<Progress value={batch?.percent??phase?.percent??0} aria-label={batch?`${batch.label} verified progress`:`${phase?.label} progress`} className="job-batch-meter"/>}{batch&&phase?.detail&&<p className="job-batch-detail" role="status">{phase.detail}</p>}<div className="job-batch-stats"><span>{batch?`${batch.completed.toLocaleString()} / ${batch.total.toLocaleString()} files ready${batch.cached?` · ${batch.cached.toLocaleString()} reused`:''}`:phase&&phase.total>0?`${phase.completed.toLocaleString()} / ${phase.total.toLocaleString()}`:'Progress appears as work is discovered.'}</span><span>{batch?.totalBytes!==null&&batch?.totalBytes!==undefined?`${formatBytes(batch.readyBytes)} / ${formatBytes(batch.totalBytes)} ready`:''}</span></div></div>

  </section>;
}
