from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from seecut_server.app import create_service
from seecut_server.config import Config
from seecut_server.service import ApiError


class AccountBoundariesTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        root = Path(self.directory.name)
        with patch.dict(os.environ, {
            "SECUT_ENV": "test",
            "SECUT_DATABASE_PATH": str(root / "database.sqlite3"),
            "SECUT_STORAGE_PATH": str(root / "storage"),
            "SECUT_EXPOSE_TEST_TOKENS": "true",
        }, clear=True):
            self.service = create_service(Config.from_env())
        self.owner = self.user("owner@example.test")
        self.member = self.user("member@example.test")
        self.outsider = self.user("outsider@example.test")
        self.team = self.service.create_team(self.owner["user"]["id"], "Shared library")["id"]

    def tearDown(self):
        self.directory.cleanup()

    def user(self, email):
        registered = self.service.register(email, "test-password-long-enough")
        self.service.verify_email(registered["verification_token"])
        return self.service.login(email, "test-password-long-enough")

    def join(self):
        invite = self.service.create_invite(self.owner["user"]["id"], self.team)
        self.service.accept_invite(self.member["user"]["id"], invite["token"])

    def test_concurrent_invite_accepts_only_one_person(self):
        invite = self.service.create_invite(self.owner["user"]["id"], self.team)

        def accept(user):
            try:
                self.service.accept_invite(user["user"]["id"], invite["token"])
                return True
            except ApiError as error:
                self.assertEqual(error.code, "INVITE_UNAVAILABLE")
                return False

        with ThreadPoolExecutor(max_workers=2) as executor:
            results = list(executor.map(accept, [self.member, self.outsider]))
        self.assertEqual(sum(results), 1)
        self.assertEqual(len(self.service.list_members(self.owner["user"]["id"], self.team)), 2)

    def test_outsider_cannot_list_or_prepare_team_upload(self):
        outsider = self.outsider["user"]["id"]
        with self.assertRaises(ApiError):
            self.service.list_assets(outsider, self.team)
        with self.assertRaises(ApiError):
            self.service.prepare_upload(outsider, "team_asset", "photo.png", "image/png", 10, team_id=self.team)

    def test_team_asset_content_is_bound_to_route_team(self):
        outsider_id = self.outsider["user"]["id"]
        other_team = self.service.create_team(outsider_id, "Other library")["id"]
        upload = self.service.prepare_upload(
            outsider_id,
            "team_asset",
            "private.png",
            "image/png",
            3,
            team_id=other_team,
        )
        self.service.save_upload(upload["upload_id"], b"png", "image/png")
        asset = self.service.complete_team_asset(outsider_id, other_team, upload["upload_id"])

        path, _, _ = self.service.storage_asset(other_team, asset["id"])
        self.assertEqual(path.read_bytes(), b"png")
        with self.assertRaises(ApiError) as context:
            self.service.storage_asset(self.team, asset["id"])
        self.assertEqual(context.exception.code, "ASSET_NOT_FOUND")

    def test_removed_member_cannot_finalize_existing_upload(self):
        self.join()
        member = self.member["user"]["id"]
        upload = self.service.prepare_upload(member, "team_asset", "photo.png", "image/png", 10, team_id=self.team)
        self.service.remove_member(self.owner["user"]["id"], self.team, member)
        with self.assertRaises(ApiError):
            self.service.complete_team_asset(member, self.team, upload["upload_id"])
        with self.assertRaises(ApiError):
            self.service.list_assets(member, self.team)

    def test_member_cannot_manage_invites_or_remove_owner(self):
        self.join()
        member = self.member["user"]["id"]
        with self.assertRaises(ApiError):
            self.service.create_invite(member, self.team)
        with self.assertRaises(ApiError):
            self.service.remove_member(member, self.team, self.owner["user"]["id"])

    def test_password_reset_revokes_old_sessions_and_token(self):
        reset = self.service.forgot_password("member@example.test")
        token = reset["reset_token"]
        self.service.reset_password(token, "replacement-password-long")
        with self.assertRaises(ApiError):
            self.service.authenticate(self.member["access_token"])
        with self.assertRaises(ApiError):
            self.service.reset_password(token, "another-password-long")
        login = self.service.login("member@example.test", "replacement-password-long")
        self.assertEqual(login["user"]["id"], self.member["user"]["id"])
