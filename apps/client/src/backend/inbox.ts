import { call } from './remote'
export type InboxRule = { id:string; origin?:string; filename_suffix?:string; dest_dir?:string; filename_prefix?:string }
export type InboxPackage = { id:string; revision:number; untrusted_name:string; default_dest_dir:string|null; rules?:InboxRule[]; archived:boolean; pending_entries:number; enqueued_entries:number; active_batch:string|null }
export type InboxEntry = { id:string; redacted_url:string; untrusted_label:string|null; untrusted_filename:string|null; dest_dir:string|null; job_id:string|null; error:{code:string;message:string}|null }
export type Page<T> = {items:T[];next_cursor:string|null}
export type PackagePage = {package:InboxPackage;entries:Page<InboxEntry>;previews:InboxPreview[];submissions:InboxDispatch[]}
export type InboxPreview = {preview_id:string;package_id:string;package_revision:number;expires_at:string;entries:{entry_id:string;redacted_url:string;filename:string|null;matched_rule?:string;dest_path:string|null;approval_needed:boolean;warnings:string[];error:{code:string;message:string}|null}[];ready_entries:number;remaining_pending:number}
export type InboxSubmission = {batch_id:string;remaining_pending:number;entries:{entry_id:string;result:{job_id?:string;approval_id?:string;error?:{code:string}}}[]}
export type InboxDispatch = {batch_id:string;state:'pending'|'complete';preview_id:string;result:InboxSubmission|null;error_code:string|null}
export const inboxApi = {
  request:<T>(operation:string,params:unknown)=>call<T>('POST',`/native/v1/inbox/${operation}`,params),
  list:(cursor?:string,archived=false)=>inboxApi.request<Page<InboxPackage>>('packages-list',{limit:100,...(cursor?{cursor}:{}),include_archived:archived}),
  get:(id:string,cursor?:string,history=false)=>inboxApi.request<PackagePage>('package-get',{package_id:id,limit:100,...(cursor?{cursor}:{}),include_history:history}),
}
