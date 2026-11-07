import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from neuromita_ai_sdk.constraints import validate_constraints, enforce_constraints
from neuromita_ai_sdk.host import PluginHost


class ConstraintTests(unittest.TestCase):
    def restriction(self, severity="forbidden"):
        return {"field": "settings.precision", "value": "fp16", "severity": severity,
                "code": "unsupported_fp16", "message": "FP16 is forbidden on this device"}

    def test_forbidden_is_enforced_without_rewriting(self):
        settings = {"precision": "fp16"}
        with self.assertRaisesRegex(ValueError, "unsupported_fp16"):
            enforce_constraints([self.restriction()], settings, None)
        self.assertEqual(settings, {"precision": "fp16"})
        enforce_constraints([self.restriction("warning")], settings, None)
        enforce_constraints([self.restriction()], {"precision": "fp32"}, None)

    def test_malformed_contract_is_rejected(self):
        with self.assertRaises(ValueError):
            validate_constraints([dict(self.restriction(), severity="maybe")])

    def test_host_rechecks_before_plugin_initialization(self):
        plugin = Mock()
        plugin.settings_schema.return_value = {"precision": {"type": "string", "default": "fp16"}}
        plugin.configuration_constraints.return_value = [self.restriction()]
        host = PluginHost(Path("nonexistent-fixture-directory"))
        with patch.object(host, "_load", return_value=plugin), \
                patch.object(host, "_manifest", return_value={"kind": "tts"}), \
                patch("neuromita_ai_sdk.host.hardware.probe", return_value={"devices": []}):
            with self.assertRaisesRegex(ValueError, "unsupported_fp16"):
                host.dispatch("initialize", {"plugin_id": "fixture", "settings": {"precision": "fp16"}})
            with self.assertRaisesRegex(ValueError, "unsupported_fp16"):
                host.dispatch("initialize", {"plugin_id": "fixture", "settings": {}})
        plugin.initialize.assert_not_called()
        self.assertEqual(host.active_by_kind, {})

    def test_inspection_reports_restrictions_without_initializing(self):
        plugin = Mock()
        plugin.settings_schema.return_value = {}
        plugin.configuration_constraints.return_value = [self.restriction()]
        host = PluginHost(Path("nonexistent-fixture-directory"))
        with patch.object(host, "_load_install_contract", return_value=plugin), \
                patch("neuromita_ai_sdk.host.hardware.probe", return_value={"devices": []}):
            result = host.dispatch("configuration_constraints", {"plugin_id": "fixture", "settings": {}})
        self.assertEqual(result, [self.restriction()])
        plugin.initialize.assert_not_called()

    def test_runtime_plugin_cannot_relax_or_bypass_install_contract_restrictions(self):
        plugin, contract = Mock(), Mock()
        for provider in (plugin, contract):
            provider.settings_schema.return_value = {"precision": {"type": "string"}}
        contract.configuration_constraints.return_value = [self.restriction("warning")]
        plugin.configuration_constraints.return_value = [self.restriction()]
        host = PluginHost(Path("split-fixture-directory"))
        with patch.object(host, "_load", return_value=plugin), \
                patch.object(host, "_load_install_contract", return_value=contract), \
                patch.object(host, "_manifest", return_value={"kind": "tts"}), \
                patch("pathlib.Path.exists", return_value=True), \
                patch("neuromita_ai_sdk.host.hardware.probe", return_value={}):
            with self.assertRaisesRegex(ValueError, "unsupported_fp16"):
                host.dispatch("initialize", {"plugin_id": "fixture", "settings": {"precision": "fp16"}})
        contract.configuration_constraints.assert_called_once()
        plugin.configuration_constraints.assert_called_once()
        plugin.initialize.assert_not_called()
