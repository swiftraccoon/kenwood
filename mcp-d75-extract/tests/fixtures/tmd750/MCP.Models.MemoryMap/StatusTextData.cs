public class StatusTextData
{
	private int m_b;

	private string c;

	public int OffsetProgrammableMemoryAddress
	{
		set
		{
			this.m_b = value;
		}
	}

	public string StatusText
	{
		get { return c; }
	}

	public StatusTextData(int A_0)
	{
	}

	public void b(n7 A_0, int A_1)
	{
		int num = ((A_1 == 4) ? (329728 + this.m_b) : (329504 + this.m_b + 48 * A_1));
		A_0.d(StatusText, num, oc.c);
	}

	public void a(n7 A_0, int A_1)
	{
		int num = ((A_1 == 4) ? (329728 + this.m_b) : (329504 + this.m_b + 48 * A_1));
		c = A_0.g(num, oc.c);
	}
}
