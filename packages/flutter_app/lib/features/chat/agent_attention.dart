import '../../core/json.dart';

/// utils/agent-attention.ts AgentAttentionClearTrigger.
enum AgentAttentionClearTrigger { focusEntry, inputFocus, promptSend, agentBlur }

/// utils/agent-attention.ts shouldClearAgentAttention: only a connected,
/// attention-requiring agent clears, never for a pending permission, and a
/// focus entry is skipped while the attention arrived during viewing.
bool shouldClearAgentAttention({
  required String? agentId,
  required bool connected,
  required bool requiresAttention,
  required String? attentionReason,
  required AgentAttentionClearTrigger trigger,
  bool hasDeferredFocusEntryClear = false,
}) {
  if (agentId == null || agentId.trim().isEmpty) return false;
  if (!connected) return false;
  if (!requiresAttention) return false;
  if (attentionReason == 'permission') return false;
  if (trigger == AgentAttentionClearTrigger.focusEntry && hasDeferredFocusEntryClear) {
    return false;
  }
  return true;
}

/// utils/agent-attention.ts ATTENTION_REASON_PRIORITY.
const _attentionReasonPriority = {'permission': 0, 'error': 1, 'finished': 2};

/// utils/agent-attention.ts pickAttentionAgent: among top-level agents that
/// require attention for a known reason, the one with the most urgent reason
/// (permission, then error, then finished), the oldest attention first; the
/// first listed wins a tie. Navigating to a workspace without a target opens
/// this agent (navigation.ts navigateToWorkspace).
String? pickAttentionAgent(Iterable<JsonMap> agents) {
  String? selectedAgentId;
  var selectedPriority = double.infinity;
  var selectedTimestamp = double.infinity;
  for (final agent in agents) {
    if (agent['requiresAttention'] != true) continue;
    final parentAgentId = agent['parentAgentId'];
    if (parentAgentId is String && parentAgentId.isNotEmpty) continue;
    final priority = _attentionReasonPriority[agent['attentionReason']];
    if (priority == null) continue;
    final timestamp = DateTime.tryParse(stringValue(agent['attentionTimestamp']))
            ?.millisecondsSinceEpoch
            .toDouble() ??
        double.infinity;
    final isHigherPriority = priority < selectedPriority;
    final isOlderAtSamePriority =
        priority == selectedPriority && timestamp < selectedTimestamp;
    if (isHigherPriority || isOlderAtSamePriority) {
      selectedAgentId = stringValue(agent['id']);
      selectedPriority = priority.toDouble();
      selectedTimestamp = timestamp;
    }
  }
  return selectedAgentId;
}

/// The facts hooks/use-agent-attention-clear.ts reads on every render.
typedef AgentAttentionState = ({
  String? agentId,
  bool connected,
  bool requiresAttention,
  String? attentionReason,
  bool screenFocused,
  bool appVisible,
});

/// hooks/use-agent-attention-clear.ts: clears the attention flag of the one
/// agent its panel shows — when the panel's pane gains focus (or the app
/// resumes into it), when its input is focused, when a prompt is sent, and
/// when the focused panel is left. Feed every state change through [update];
/// [clear] sends `clear_agent_attention` for the agent id.
class AgentAttentionClear {
  AgentAttentionClear(AgentAttentionState initial, this.clear)
      : _state = initial,
        _prevRequiresAttention = initial.requiresAttention,
        _prevActivelyViewed = initial.screenFocused && initial.appVisible,
        _prevAppVisible = initial.appVisible {
    _effects();
  }

  final void Function(String agentId) clear;
  AgentAttentionState _state;
  bool _deferredFocusEntryClear = false;
  bool _prevRequiresAttention;
  bool _prevActivelyViewed;
  bool _prevScreenFocused = false;
  bool _prevAppVisible;

  AgentAttentionState get state => _state;

  void update(AgentAttentionState next) {
    _state = next;
    _effects();
  }

  void clearOnInputFocus() => _clear(AgentAttentionClearTrigger.inputFocus);
  void clearOnPromptSend() => _clear(AgentAttentionClearTrigger.promptSend);
  void clearOnAgentBlur() => _clear(AgentAttentionClearTrigger.agentBlur);

  /// The hook's effects, in their declaration order.
  void _effects() {
    final s = _state;
    if (!s.requiresAttention) _deferredFocusEntryClear = false;

    final activelyViewed = s.screenFocused && s.appVisible;
    if (!_prevRequiresAttention &&
        s.requiresAttention &&
        _prevActivelyViewed &&
        activelyViewed) {
      _deferredFocusEntryClear = true;
    }
    _prevRequiresAttention = s.requiresAttention;
    _prevActivelyViewed = activelyViewed;

    final enteredScreenFocus = !_prevScreenFocused && s.screenFocused && s.appVisible;
    final resumedIntoFocusedAgent = !_prevAppVisible && s.appVisible && s.screenFocused;
    _prevScreenFocused = s.screenFocused;
    _prevAppVisible = s.appVisible;
    if (enteredScreenFocus || resumedIntoFocusedAgent) {
      _clear(AgentAttentionClearTrigger.focusEntry);
    }
  }

  void _clear(AgentAttentionClearTrigger trigger) {
    final s = _state;
    final id = s.agentId?.trim();
    if (id == null || id.isEmpty) return;
    if (!shouldClearAgentAttention(
      agentId: id,
      connected: s.connected,
      requiresAttention: s.requiresAttention,
      attentionReason: s.attentionReason,
      trigger: trigger,
      hasDeferredFocusEntryClear: _deferredFocusEntryClear,
    )) {
      return;
    }
    _deferredFocusEntryClear = false;
    clear(id);
  }
}
