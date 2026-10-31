// Executed by Pi's extension loader, never by a Paseo Node daemon.
import { readFileSync } from 'node:fs';
const config = JSON.parse(readFileSync(new URL('./runtime.json', import.meta.url), 'utf8'));
const decode = value => JSON.parse(Buffer.from(value.trim(), 'base64url').toString('utf8'));
const text = value => typeof value === 'string' ? value : Array.isArray(value) ? value.filter(p => p?.type === 'text' && typeof p.text === 'string').map(p => p.text).join('\n\n') : '';
const entry = e => ({id:e.id, parentId:e.parentId ?? null, text:text(e.message.content)});
const entries = values => values.filter(e => e.type === 'message' && e.message?.role === 'user').map(entry);
function capture(ctx, reason, requestId) {
  ctx.ui.notify('PASEO_ENTRY_CAPTURE ' + JSON.stringify({reason,requestId,treeEntries:entries(ctx.sessionManager.getEntries()),contextEntries:entries(ctx.sessionManager.buildContextEntries())}), 'info');
}
export default function paseoIntegration(pi) {
  for (const [name, server] of Object.entries(config.mcpServers ?? {})) pi.registerMcpServer(name, server);
  const submitted = [];
  let gotgenesParent;
  pi.on('session_start', (_event, ctx) => {
    gotgenesParent = {ctx, service:globalThis[Symbol.for('@gotgenes/pi-subagents:service')]};
    capture(ctx, 'session_start');
  });
  pi.events.on('subagents:child:session-created', event => {
    const parent = gotgenesParent;
    let attempts = 0;
    const report = () => {
      const record = parent?.service?.listAgents().find(agent => agent.outputFile?.includes(event.sessionId));
      if (record) parent.ctx.ui.notify('PASEO_GOTGENES_CHILD_SESSION ' + JSON.stringify({agentId:record.id,file:record.outputFile}), 'info');
      else if (++attempts < 240) setTimeout(report, 250);
    };
    setTimeout(report, 0);
  });
  const reportSubmitted = ctx => {
    const all = ctx.sessionManager.getEntries();
    for (let i = 0; i < submitted.length; i++) {
      const found = all.find(e => e.type === 'message' && e.message === submitted[i]);
      if (!found) continue;
      submitted.splice(i--, 1);
      ctx.ui.notify('PASEO_SUBMITTED_USER_ENTRY ' + JSON.stringify({entry:entry(found)}), 'info');
    }
  };
  if (config.systemPrompt) pi.on('before_agent_start', async event => ({systemPrompt:event.systemPrompt + '\n\n' + config.systemPrompt}));
  pi.on('message_end', async event => { if (event.message?.role === 'user') submitted.push(event.message); });
  pi.on('message_start', async (event, ctx) => { if (event.message?.role === 'assistant') reportSubmitted(ctx); });
  pi.on('turn_end', async (_event, ctx) => { reportSubmitted(ctx); capture(ctx, 'turn_end'); });
  pi.registerCommand('paseo_capture_entries', {description:'Internal Paseo entry capture bridge', handler:async (args, ctx) => capture(ctx, 'command', decode(args).requestId)});
  pi.registerCommand('paseo_tree', {description:'Internal Paseo tree navigation bridge', handler:async (args, ctx) => {
    const payload = decode(args);
    try {
      const result = await ctx.navigateTree(payload.targetId, {summarize:false});
      pi.appendEntry('paseo_rewind', {targetId:payload.targetId});
      capture(ctx, 'tree_navigation');
      ctx.ui.notify('PASEO_COMMAND_RESULT ' + JSON.stringify({requestId:payload.requestId,ok:true,result}), 'info');
    } catch (error) {
      ctx.ui.notify('PASEO_COMMAND_RESULT ' + JSON.stringify({requestId:payload.requestId,ok:false,error:error instanceof Error ? error.message : String(error)}), 'error');
      throw error;
    }
  }});
}
