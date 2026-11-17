import React from 'react';
import { Check, ChevronDown, ChevronUp, CircleAlert, LoaderCircle, X } from 'lucide-react';
import { Button } from './ui/button';
import { Progress } from './ui/progress';
import { formatBytes, formatPercent, type JobProgress, type TaskProgress } from '../lib/job-progress';

function TaskRow({task}:{task:TaskProgress}) {
  const name=task.name.split('/').pop()||task.name;
  const directory=task.name.includes('/')?task.name.slice(0,task.name.lastIndexOf('/')):'';
  const transferring=task.state==='downloading'||task.state==='uploading'||task.state==='verifying';
  const percent=task.totalBytes!==null&&task.totalBytes>0?Math.min(100,task.bytes/task.totalBytes*100):null;
  const labels={queued:'Waiting',checking:'Checking cache',running:'Running',downloading:'Downloading',uploading:'Uploading',verifying:'Verifying',retrying:`Retrying · attempt ${task.attempt+1}`,complete:task.cached?'Reused':'Ready',failed:'Failed',cancelled:'Stopped'};
  return <li className={`job-task task-${task.state}`} title={task.error||task.name}>
    <div className="job-task-heading"><span className="job-task-name" title={task.name}>{name}</span><span className="job-task-state">{labels[task.state]}</span></div>
    <div className="job-task-detail"><span title={directory}>{directory||' '}</span><span>{transferring?`${formatBytes(task.bytes)}${task.totalBytes!==null?` / ${formatBytes(task.totalBytes)}`:''}`:task.state==='complete'&&task.totalBytes!==null?formatBytes(task.totalBytes):''}</span></div>
    <div className="job-task-meter">{transferring&&<Progress aria-label={`${task.name} transfer`} value={percent??undefined} className={percent===null?'indeterminate':''}/>}</div>
  </li>;
}

export function JobProgressPanel({progress,onCancel}:{progress:JobProgress|null;onCancel:()=>void}) {
  const [expandedJob,setExpandedJob]=React.useState<number|null>(null);
  const expanded=progress!==null&&expandedJob===progress.jobId;
  const setExpanded=(value:boolean)=>setExpandedJob(value?progress?.jobId??null:null);
  const phase=progress?.phases[progress.activePhase];
  const batch=phase?.batch;
  const settled=progress?.phases.filter(p=>p.state==='complete'||p.state==='skipped').length??0;
  const cancelling=progress?.state==='cancelling';
  const stopped=progress?.state==='failed'||progress?.state==='cancelled';
  const tasks=progress?.tasks??[];
  const recent=tasks.length<4?(progress?.recent??[]).filter(task=>!tasks.some(active=>active.id===task.id)):[];
  return <section className="job-progress-panel" aria-label="Operation progress">
    <div className="job-overview"><div className="job-title"><span className="job-status-icon">{stopped?<CircleAlert size={17}/>:progress?.state==='complete'?<Check size={17}/>:<LoaderCircle size={17} className="animate-spin"/>}</span><div><strong>{progress?.title??'Getting ready'}</strong><p role="status" aria-live="polite">{cancelling?'Stopping safely…':stopped?progress?.state==='failed'?'Operation failed':'Operation cancelled':phase?`Step ${progress!.activePhase+1} of ${progress!.phases.length} · ${phase.label}`:'Starting operation…'}</p></div></div><div className="job-controls"><span className="job-step-count">{progress?`${settled} / ${progress.phases.length} steps`:''}</span><Button variant="ghost" size="sm" aria-expanded={expanded} aria-controls="job-details" onClick={()=>setExpanded(!expanded)}>{expanded?<ChevronDown/>:<ChevronUp/>}{expanded?'Hide tasks':'Show tasks'}</Button><Button size="sm" variant="outline" onClick={onCancel} disabled={cancelling||stopped||progress?.state==='complete'}><X/>{cancelling?'Stopping':'Cancel'}</Button></div></div>
    {!!progress?.phases.length&&<ol className="job-phases" style={{gridTemplateColumns:`repeat(${progress.phases.length}, minmax(0, 1fr))`}} aria-label="Installation steps">{progress.phases.map((step,index)=><li key={step.id} className={`job-phase phase-${step.state}`} aria-current={step.state==='running'?'step':undefined} title={`${step.label}: ${step.state}`}><div className="job-phase-track" role="progressbar" aria-label={step.label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={step.percent??undefined} aria-valuetext={step.state==='running'?(step.percent===null?'Working':`${formatPercent(step.percent)} verified`):step.state}><span className={`job-phase-fill ${step.state==='running'&&step.percent===null?'phase-working':''}`} style={{width:`${step.percent??0}%`}}/></div><span className="job-phase-label"><span>{step.state==='complete'?<Check size={10}/>:step.state==='skipped'?'—':index+1}</span>{step.label}</span></li>)}</ol>}
    <div className="job-batch"><div className="job-batch-main"><span>{batch?batch.label:phase?.detail||phase?.label||'Preparing work'}</span>{batch&&<strong>{formatPercent(batch.percent)} <small>{batch.basis==='bytes'?'of bytes':'of files'} verified</small></strong>}</div>{batch&&<Progress value={batch.percent} aria-label={`${batch.label} verified progress`} className="job-batch-meter"/>}<div className="job-batch-stats"><span>{batch?`${batch.completed.toLocaleString()} / ${batch.total.toLocaleString()} files ready${batch.cached?` · ${batch.cached.toLocaleString()} reused`:''}`:phase&&phase.total>0?`${phase.completed.toLocaleString()} / ${phase.total.toLocaleString()}`:'Progress appears as work is discovered.'}</span><span>{batch?.totalBytes!==null&&batch?.totalBytes!==undefined?`${formatBytes(batch.readyBytes)} / ${formatBytes(batch.totalBytes)} ready`:''}</span></div></div>
    {expanded&&<div id="job-details" className="job-details"><div className="job-details-heading"><span>{phase?.active??0} active{phase?.queued?` · ${phase.queued.toLocaleString()} queued`:''}</span><span>{progress?.uploadedBytes?`${formatBytes(progress.uploadedBytes)} uploaded · `:''}{formatBytes(progress?.downloadedBytes??0)} downloaded this operation</span></div><ul className="job-tasks" aria-label="Parallel jobs">{tasks.map(task=><TaskRow key={task.id} task={task}/>)}{recent.map(task=><TaskRow key={task.id} task={task}/>)}{tasks.length===0&&recent.length===0&&<li className="job-no-transfers">{phase?.detail||'Reading metadata or processing local files…'}<span>This step has no measurable download total yet.</span></li>}</ul><p className="job-progress-note">Step bars advance after verification. Each transfer shows its own current bytes.</p></div>}
  </section>;
}
