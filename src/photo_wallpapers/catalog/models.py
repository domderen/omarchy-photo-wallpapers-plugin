from dataclasses import dataclass, field


@dataclass(frozen=True)
class Asset:
    id: str
    filename: str
    revision: str
    width: int
    height: int
    captured_at: str | None = None
    orientation: int = 1
    media_kind: str = "image"
    favorite: bool = False
    keywords: tuple[str, ...] = ()
    rendition: str = "original"
    burst_id: str | None = None


@dataclass(frozen=True)
class Person:
    id: str
    name: str


@dataclass(frozen=True)
class Collection:
    id: str
    kind: str
    name: str
    available: bool = True


@dataclass(frozen=True)
class Face:
    asset_id: str
    person_id: str | None
    x: float | None = None
    y: float | None = None
    width: float | None = None
    height: float | None = None
    coordinates: str | None = None
    rendition: str | None = None
    revision: str | None = None


@dataclass
class CatalogSnapshot:
    namespace: str
    assets: list[Asset] = field(default_factory=list)
    people: list[Person] = field(default_factory=list)
    collections: list[Collection] = field(default_factory=list)
    photo_people: list[tuple[str, str]] = field(default_factory=list)
    photo_albums: list[tuple[str, str]] = field(default_factory=list)
    faces: list[Face] = field(default_factory=list)
    capabilities: dict = field(default_factory=dict)

    enrichment: dict = field(default_factory=dict)
