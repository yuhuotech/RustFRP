import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../core/store.dart';
import '../../core/local_agent.dart';
import '../../shared/widgets.dart';
import '../../theme/tokens.dart';

/// Login. The browser build talks to the controller that served it (no address to enter); the desktop app asks
/// for the controller address once and remembers it.
class LoginPage extends ConsumerStatefulWidget {
  const LoginPage({super.key});
  @override
  ConsumerState<LoginPage> createState() => _LoginPageState();
}

class _LoginPageState extends ConsumerState<LoginPage> {
  late final TextEditingController _server;
  final _name = TextEditingController();
  final _password = TextEditingController();
  bool _busy = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    _server = TextEditingController(text: ref.read(sessionProvider).serverUrl);
  }

  Future<void> _submit() async {
    if (_busy) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await ref
        .read(sessionProvider.notifier)
        .login(_name.text, _password.text, serverUrl: _server.text);
    if (!mounted) return;
    setState(() {
      _busy = false;
      _error = err;
    });
  }

  @override
  void dispose() {
    _server.dispose();
    _name.dispose();
    _password.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = context.c;
    final sameOrigin = ref.watch(sameOriginProvider);
    final ready = _name.text.trim().isNotEmpty && _password.text.isNotEmpty;
    return Scaffold(
      backgroundColor: c.bgApp,
      body: Center(
        child: SingleChildScrollView(
          padding: const EdgeInsets.all(24),
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 360),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                const Center(child: BrandLogo(size: 56)),
                const SizedBox(height: 16),
                Text(
                  'RustFRP',
                  textAlign: TextAlign.center,
                  style: Type.page.copyWith(color: c.text1),
                ),
                const SizedBox(height: 4),
                Text(
                  '登录管理面板',
                  textAlign: TextAlign.center,
                  style: Type.body.copyWith(color: c.text3),
                ),
                const SizedBox(height: 24),
                if (!sameOrigin) ...[
                  Text('服务器地址', style: Type.caption.copyWith(color: c.text3)),
                  const SizedBox(height: 4),
                  LTextField(
                    key: const Key('login-server'),
                    controller: _server,
                    hint: 'https://frp.example.com',
                    prefix: Icon(LucideIcons.server, size: 14, color: c.text3),
                    onChanged: (_) => setState(() {}),
                  ),
                  const SizedBox(height: 12),
                ],
                Text('用户名', style: Type.caption.copyWith(color: c.text3)),
                const SizedBox(height: 4),
                LTextField(
                  key: const Key('login-name'),
                  controller: _name,
                  autofocus: true,
                  prefix: Icon(LucideIcons.user, size: 14, color: c.text3),
                  onChanged: (_) => setState(() {}),
                ),
                const SizedBox(height: 12),
                Text('密码', style: Type.caption.copyWith(color: c.text3)),
                const SizedBox(height: 4),
                LTextField(
                  key: const Key('login-password'),
                  controller: _password,
                  obscure: true,
                  prefix: Icon(LucideIcons.lock, size: 14, color: c.text3),
                  onChanged: (_) => setState(() {}),
                  onSubmitted: (_) => ready ? _submit() : null,
                ),
                if (_error != null) ...[
                  const SizedBox(height: 12),
                  Container(
                    padding: const EdgeInsets.all(10),
                    decoration: BoxDecoration(
                      color: c.dangerSoft,
                      borderRadius: BorderRadius.circular(Radii.control),
                    ),
                    child: Text(
                      _error!,
                      key: const Key('login-error'),
                      style: Type.body.copyWith(color: c.dangerText),
                    ),
                  ),
                ],
                const SizedBox(height: 20),
                LButton(
                  key: const Key('login-submit'),
                  label: '登录',
                  variant: BtnVariant.solid,
                  loading: _busy,
                  onPressed: ready ? _submit : null,
                ),
                if (ref.watch(localAvailableProvider)) ...[
                  const SizedBox(height: 12),
                  LButton(
                    key: const Key('local-mode'),
                    label: '管理本机',
                    icon: LucideIcons.monitor,
                    onPressed: () =>
                        ref.read(localModeProvider.notifier).select(true),
                  ),
                ],
                const SizedBox(height: 16),
                Text(
                  '管理员账号只能在服务器上用 rustfrp-server admin create 创建。',
                  textAlign: TextAlign.center,
                  style: Type.caption.copyWith(color: c.text3),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
