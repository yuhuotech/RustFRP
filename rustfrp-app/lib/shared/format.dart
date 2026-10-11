/// Small formatting helpers (Chinese UI). All times are UTC milliseconds from the server.
library;

String two(int n) => n.toString().padLeft(2, '0');

/// `刚刚` / `3 分钟前` / `2 小时前` / `昨天 10:18` / `3月14日 10:18`.
String relativeTime(int? ms, {int? nowMs}) {
  if (ms == null) return '从未';
  final now = DateTime.fromMillisecondsSinceEpoch(
    nowMs ?? DateTime.now().millisecondsSinceEpoch,
  );
  final t = DateTime.fromMillisecondsSinceEpoch(ms);
  final d = now.difference(t);
  if (d.inSeconds < 45) return '刚刚';
  if (d.inMinutes < 60) return '${d.inMinutes <= 0 ? 1 : d.inMinutes} 分钟前';
  if (d.inHours < 24 && now.day == t.day) return '${d.inHours} 小时前';
  final hm = '${two(t.hour)}:${two(t.minute)}';
  final yesterday = DateTime(
    now.year,
    now.month,
    now.day,
  ).subtract(const Duration(days: 1));
  if (DateTime(t.year, t.month, t.day) == yesterday) return '昨天 $hm';
  if (t.year == now.year) return '${t.month}月${t.day}日 $hm';
  return '${t.year}年${t.month}月${t.day}日 $hm';
}

/// `2026-10-11 09:30:12`.
String fullTime(int ms) {
  final t = DateTime.fromMillisecondsSinceEpoch(ms);
  return '${t.year}-${two(t.month)}-${two(t.day)} ${two(t.hour)}:${two(t.minute)}:${two(t.second)}';
}

/// Remaining time until [ms]: `45 分钟` / `2 小时` / `已过期`.
String remaining(int ms, {int? nowMs}) {
  final left = ms - (nowMs ?? DateTime.now().millisecondsSinceEpoch);
  if (left <= 0) return '已过期';
  final m = (left / 60000).ceil();
  if (m < 60) return '$m 分钟';
  return '${(m / 60).ceil()} 小时';
}

/// `ssh -p 6000 host` style address.
String hostPort(String host, int port) =>
    host.contains(':') && !host.startsWith('[')
    ? '[$host]:$port'
    : '$host:$port';

String osLabel(String os) => switch (os) {
  'macos' => 'macOS',
  'linux' => 'Linux',
  'windows' => 'Windows',
  '' => '未知系统',
  _ => os,
};

/// Binary units match the bytes counted by Edge.
String formatBytes(int bytes) {
  var value = bytes < 0 ? 0.0 : bytes.toDouble();
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  var unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return '${unit == 0 ? value.toInt() : value.toStringAsFixed(1)} ${units[unit]}';
}

/// 运行时长，如「3 天 4 小时」「5 分钟」「12 秒」；只保留最大的两级单位。
String formatDuration(int seconds) {
  final s = seconds < 0 ? 0 : seconds;
  final (d, h, m) = (s ~/ 86400, s % 86400 ~/ 3600, s % 3600 ~/ 60);
  if (d > 0) return h == 0 ? '$d 天' : '$d 天 $h 小时';
  if (h > 0) return m == 0 ? '$h 小时' : '$h 小时 $m 分钟';
  if (m > 0) return '$m 分钟';
  return '$s 秒';
}
