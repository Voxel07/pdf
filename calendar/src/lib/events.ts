export interface EventInput {
  title: string;
  description: string;
  location: string;
  starts_at: string;
  ends_at: string;
  all_day: boolean;
  status?: 'scheduled' | 'cancelled';
}

export interface CalendarEvent extends EventInput {
  status: NonNullable<EventInput['status']>;
  id: string;
  revision: number;
  created_at: string;
  updated_at: string;
}

export interface CalendarSettings {
  name: string;
  feed_url: string;
}
