export type JobState = 'running'|'cancelling'|'complete'|'failed'|'cancelled';
export type TaskState = 'queued'|'checking'|'running'|'downloading'|'uploading'|'verifying'|'retrying'|'complete'|'failed'|'cancelled';
export type TaskProgress = {id:string;name:string;state:TaskState;bytes:number;totalBytes:number|null;attempt:number;cached:boolean;error:string|null};
export type BatchProgress = {label:string;basis:'bytes'|'files';completed:number;total:number;cached:number;readyBytes:number;totalBytes:number|null;percent:number};
export type PhaseProgress = {id:string;label:string;state:'pending'|'running'|'complete'|'skipped'|'failed'|'cancelled';detail:string;completed:number;total:number;percent:number|null;batch:BatchProgress|null;active:number;queued:number};
export type JobProgress = {jobId:number;revision:number;title:string;state:JobState;phases:PhaseProgress[];activePhase:number;tasks:TaskProgress[];recent:TaskProgress[];downloadedBytes:number;uploadedBytes:number;error:string|null};
export function remainingPhases(phases:PhaseProgress[]) {
  return phases.map((phase,index)=>({phase,index})).filter(({phase})=>phase.state!=='complete'&&phase.state!=='skipped');
}
export function isNewerProgress(next:JobProgress, previous:Pick<JobProgress,'jobId'|'revision'>|null):boolean {
  return !previous || next.jobId>previous.jobId || next.jobId===previous.jobId&&next.revision>previous.revision;
}
export function formatBytes(value:number):string {
  if(value<1024)return `${Math.floor(value)} B`;
  const unit=value>=1024**3?3:value>=1024**2?2:1;
  return `${(value/1024**unit).toFixed(1)} ${['B','KB','MB','GB'][unit]}`;
}
export function formatPercent(value:number):string {
  const clamped=Math.max(0,Math.min(100,value));
  // A rounded 99.9 must never claim that verification finished.
  return `${clamped>0&&clamped<1?clamped.toFixed(1):Math.floor(clamped)}%`;
}
