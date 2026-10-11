import 'package:flutter_test/flutter_test.dart';
import 'package:rustfrp_app/shared/format.dart';

void main() {
  final now = DateTime(2026, 10, 11, 12, 0).millisecondsSinceEpoch;
  int ago(Duration d) => now - d.inMilliseconds;

  test('relativeTime', () {
    expect(relativeTime(null), '从未');
    expect(relativeTime(ago(const Duration(seconds: 10)), nowMs: now), '刚刚');
    expect(relativeTime(ago(const Duration(minutes: 5)), nowMs: now), '5 分钟前');
    expect(relativeTime(ago(const Duration(minutes: 50)), nowMs: now), '50 分钟前');
    expect(relativeTime(ago(const Duration(hours: 3)), nowMs: now), '3 小时前');
    expect(relativeTime(DateTime(2026, 10, 10, 10, 18).millisecondsSinceEpoch, nowMs: now), '昨天 10:18');
    expect(relativeTime(DateTime(2026, 3, 14, 9, 5).millisecondsSinceEpoch, nowMs: now), '3月14日 09:05');
    expect(relativeTime(DateTime(2025, 12, 31, 23, 59).millisecondsSinceEpoch, nowMs: now), '2025年12月31日 23:59');
    // Spanning midnight: 5 hours ago but on the previous calendar day.
    expect(relativeTime(DateTime(2026, 10, 10, 23, 0).millisecondsSinceEpoch, nowMs: now), '昨天 23:00');
    expect(relativeTime(now - 1, nowMs: now), '刚刚');
    expect(relativeTime(ago(const Duration(minutes: 1)), nowMs: now), '1 分钟前');
  });

  test('fullTime pads fields', () {
    expect(fullTime(DateTime(2026, 1, 2, 3, 4, 5).millisecondsSinceEpoch), '2026-01-02 03:04:05');
    expect(two(7), '07');
  });

  test('remaining', () {
    expect(remaining(now - 1, nowMs: now), '已过期');
    expect(remaining(now + 30 * 60000, nowMs: now), '30 分钟');
    expect(remaining(now + 90 * 60000, nowMs: now), '2 小时');
    expect(remaining(now + 1, nowMs: now), '1 分钟');
  });

  test('hostPort and osLabel', () {
    expect(hostPort('h', 1), 'h:1');
    expect(hostPort('::1', 1), '[::1]:1');
    expect(hostPort('[::1]', 1), '[::1]:1');
    expect(osLabel('macos'), 'macOS');
    expect(osLabel('linux'), 'Linux');
    expect(osLabel('windows'), 'Windows');
    expect(osLabel(''), '未知系统');
    expect(osLabel('freebsd'), 'freebsd');
  });
}
