"""Read-only OSXPhotos adapter. No Photos paths enter the portable catalog."""

import hashlib
import json
from datetime import UTC
from pathlib import Path

from ..catalog.models import Asset, CatalogSnapshot, Collection, Person


class PhotosAdapter:
    def __init__(self, library_path: Path, database_factory=None):
        import logging

        logging.getLogger("osxphotos").disabled = True
        self.library_path = library_path.resolve()
        self.database_factory = database_factory
        self.database = None
        self._media = {}

    def extract(self, snapshot_path: Path, *, metadata_only=False) -> CatalogSnapshot:
        if self.database_factory is None:
            import osxphotos

            factory = osxphotos.PhotosDB
        else:
            factory = self.database_factory
        # Search databases are outside this transaction; omit their optional data.
        db = factory(
            dbfile=str(snapshot_path),
            library_path=str(self.library_path),
            _skip_searchinfo=True,
        )
        self.database = None if metadata_only else db
        result = CatalogSnapshot(
            namespace=hashlib.sha256(str(self.library_path).encode()).hexdigest(),
            capabilities={
                "people": True,
                "albums": True,
                "shared_albums": True,
                "shared_photo_library": False,
                "face_bounds": False,
                "search_analysis": False,
                "rendition": "edited-when-available; original-fallback",
            },
        )
        people = {
            p.uuid: Person(p.uuid, p.name)
            for p in db.person_info
            if p.uuid and p.name and p.name != "_UNKNOWN_"
        }
        shared_ids = {a.uuid for a in getattr(db, "album_info_shared", [])}
        albums = {
            a.uuid: Collection(
                a.uuid, "shared_album" if a.uuid in shared_ids else "album", a.title
            )
            for a in [*db.album_info, *getattr(db, "album_info_shared", [])]
        }
        media = {}
        visible = set()
        mappings = []
        for photo in db.photos(images=True, movies=False):
            if photo.hidden or photo.intrash or not photo.isphoto:
                continue
            if not photo.width or not photo.height:
                continue
            visible.add(photo.uuid)
            if not metadata_only:
                revision_data = [
                    photo.uuid,
                    photo.fingerprint,
                    str(photo.date_modified),
                    photo.width,
                    photo.height,
                    photo.orientation,
                    photo.hasadjustments,
                ]
                revision = hashlib.sha256(
                    json.dumps(revision_data).encode()
                ).hexdigest()
                result.assets.append(
                    Asset(
                        photo.uuid,
                        photo.original_filename,
                        revision,
                        photo.width,
                        photo.height,
                        str(photo.date) if photo.date else None,
                        photo.orientation or 1,
                        favorite=bool(photo.favorite),
                        keywords=tuple(sorted(photo.keywords)),
                        rendition="edited" if photo.hasadjustments else "original",
                    )
                )
            for person in photo.person_info:
                if person.uuid and person.name and person.name != "_UNKNOWN_":
                    people[person.uuid] = Person(person.uuid, person.name)
                    result.photo_people.append((photo.uuid, person.uuid))
            for album in photo.album_info:
                if album.uuid in albums:
                    result.photo_albums.append((photo.uuid, album.uuid))
            mappings.append(
                {
                    "local_id": photo.uuid,
                    "cloud_master_guid": getattr(photo, "cloud_guid", None) or None,
                    "owner_hash": getattr(photo, "cloud_owner_hashed_id", None) or None,
                    "person_ids": sorted(
                        {p.uuid for p in photo.person_info if p.uuid in people}
                    ),
                    "keywords": sorted(photo.keywords),
                }
            )
            if not metadata_only:
                media[photo.uuid] = (revision, photo)
        # Shared albums expose membership via AlbumInfo even when PhotoInfo omits it.
        for album in getattr(db, "album_info_shared", []):
            for photo in album.photos:
                if photo.uuid in visible:
                    result.photo_albums.append((photo.uuid, album.uuid))
        result.people = sorted(people.values(), key=lambda p: p.id)
        result.collections = sorted(albums.values(), key=lambda a: a.id)
        result.photo_people = sorted(set(result.photo_people))
        result.photo_albums = sorted(set(result.photo_albums))
        result.assets.sort(key=lambda a: a.id)
        from datetime import datetime

        collection_members = {}
        for local_id, collection_id in result.photo_albums:
            collection_members.setdefault(local_id, []).append(collection_id)
        for mapping in mappings:
            mapping["collection_ids"] = sorted(
                set(collection_members.get(mapping["local_id"], []))
            )
        result.enrichment = {
            "schema_version": 1,
            "provider_namespace": result.namespace,
            "created_at": datetime.now(UTC).isoformat(),
            "cloud_id_type": "CPLMaster.recordName",
            "people": [{"id": p.id, "name": p.name} for p in result.people],
            "collections": [
                {"id": c.id, "kind": c.kind, "name": c.name, "available": c.available}
                for c in result.collections
            ],
            "assets": sorted(mappings, key=lambda a: a["local_id"]),
            "capabilities": {"people": True, "face_bounds": False},
        }
        self._media = media
        return result

    def resolve(self, asset_id, revision):
        value = self._media.get(asset_id)
        if not value:
            raise KeyError("unpublished asset")
        if value[0] != revision:
            raise ValueError("stale revision")
        return value[1]

    def export(self, asset_id, revision, directory):
        photo = self.resolve(asset_id, revision)
        edited = bool(photo.hasadjustments and photo.path_edited)
        rendition = "edited" if edited else "original"
        missing = not (photo.path_edited if edited else photo.path)
        paths = photo.export(
            str(directory), edited=edited, use_photos_export=missing, timeout=60
        )
        if not paths:
            raise FileNotFoundError(
                "Photos could not prepare this asset; open Photos and allow download"
            )
        return Path(paths[0]), rendition
